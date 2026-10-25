//! A sealed generation read as the parent of a sparse successor.
//!
//! The parent is its authenticated partitioned manifest and a reader over
//! its content-addressed segments. Nothing is decoded until the successor
//! asks for one file, one file's evidence, one graph page, or one resolution
//! index page, so what a successor build holds is proportional to what it
//! reads.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};
use tracedecay_runtime_core::resident_memory::ResidentMemoryOperationV1;

use tracedecay_domain::{
    CodeGenerationId, CodeGenerationManifestV1, FileOccurrenceId, ManifestDigest,
    SanitizedCodeFileV1, SanitizedCodeSnapshotV1,
};

use super::file_evidence_rows::PersistedFileEvidenceV1;
use super::partitioned_codec::{
    PartitionedCodeGraphPageDescriptorV1, PartitionedFileEvidenceDescriptorV1,
    PartitionedFileSegmentDescriptorV1, PartitionedGenerationEvidenceV1,
    PartitionedPublishedGenerationV1, SealedGenerationSegmentReadV1, decode_file_evidence_segment,
    decode_file_segment, decode_generation_evidence, parse_partitioned_manifest,
};
use super::resolution_index::ResolutionIndexReaderV1;
use super::resolution_view::{
    CompactFileResolutionV1, ResolutionFileFactsV1, ResolutionFileV1, resolution_source_bytes,
};
use super::sealed_codec::{FileScopeIdentityV1, restore_file_pages};
use super::{
    ChunkPolicyRevisionSummaryV1, CodeIndexGenerationStatisticsV1, CodeIndexProductionErrorV1,
    CodeIndexPublishedGenerationV1, FileGenerationArtifactsV1,
    VerifiedSealedTextGenerationMetadataV1,
};

/// Reads one sealed segment's bytes into the buffer it is handed. Shared by
/// every worker a successor build fans out to.
pub type SealedSegmentReaderV1 = dyn Fn(SealedGenerationSegmentReadV1<'_>, &mut Vec<u8>) -> Result<(), CodeIndexProductionErrorV1>
    + Send
    + Sync;

/// The active generation of one scope as its publication store holds it:
/// the authenticated manifest bytes and a reader over its segments.
#[derive(Clone)]
pub struct CodeIndexSealedGenerationV1 {
    manifest_bytes: Arc<[u8]>,
    read_segment: Arc<SealedSegmentReaderV1>,
}

impl std::fmt::Debug for CodeIndexSealedGenerationV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CodeIndexSealedGenerationV1")
            .field("manifest_bytes", &self.manifest_bytes.len())
            .finish_non_exhaustive()
    }
}

impl CodeIndexSealedGenerationV1 {
    pub fn new(manifest_bytes: Arc<[u8]>, read_segment: Arc<SealedSegmentReaderV1>) -> Self {
        Self {
            manifest_bytes,
            read_segment,
        }
    }

    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest_bytes
    }

    pub fn segment_reader(&self) -> Arc<SealedSegmentReaderV1> {
        Arc::clone(&self.read_segment)
    }

    /// The authenticated manifest, snapshot, and statistics this generation
    /// seals, parsed without decoding any segment.
    pub fn metadata(
        &self,
    ) -> Result<VerifiedSealedTextGenerationMetadataV1, CodeIndexProductionErrorV1> {
        CodeIndexPublishedGenerationV1::partitioned_text_metadata(&self.manifest_bytes)
    }
}

/// A cold build's canonical file segments before its final manifest exists.
/// The source borrows the real generation identity; it creates no parent or
/// provisional sealed manifest and never retains a decoded file.
pub(super) struct StagedFileSourceV1<'a> {
    manifest: &'a CodeGenerationManifestV1,
    snapshot: &'a SanitizedCodeSnapshotV1,
    files: &'a [PartitionedFileSegmentDescriptorV1],
    read: Arc<SealedSegmentReaderV1>,
    scope: FileScopeIdentityV1,
}

impl<'a> StagedFileSourceV1<'a> {
    pub(super) fn new(
        manifest: &'a CodeGenerationManifestV1,
        snapshot: &'a SanitizedCodeSnapshotV1,
        files: &'a [PartitionedFileSegmentDescriptorV1],
        read: Arc<SealedSegmentReaderV1>,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let mut previous = None;
        for descriptor in files {
            let file = snapshot
                .files
                .get(descriptor.file_key as usize)
                .ok_or_else(|| contract("staged file segment is outside its snapshot"))?;
            if file.disposition != tracedecay_domain::SnapshotFileDispositionV1::Present
                || file.file_occurrence_id != descriptor.file_occurrence_id
                || previous.is_some_and(|key| key >= descriptor.file_key)
            {
                return Err(contract(
                    "staged file descriptors disagree with their snapshot",
                ));
            }
            previous = Some(descriptor.file_key);
        }
        if snapshot
            .files
            .iter()
            .filter(|file| {
                file.disposition == tracedecay_domain::SnapshotFileDispositionV1::Present
            })
            .count()
            != files.len()
        {
            return Err(contract(
                "staged file descriptors do not cover every present file",
            ));
        }
        Ok(Self {
            manifest,
            snapshot,
            files,
            read,
            scope: FileScopeIdentityV1::of(manifest, snapshot),
        })
    }

    pub(super) fn decode_file(
        &self,
        file_key: u32,
    ) -> Result<Arc<FileGenerationArtifactsV1>, CodeIndexProductionErrorV1> {
        let index = self
            .files
            .binary_search_by_key(&file_key, |file| file.file_key)
            .map_err(|_| contract("staged source has no such present file"))?;
        let descriptor = &self.files[index];
        let snapshot_file = self
            .snapshot
            .files
            .get(file_key as usize)
            .ok_or_else(|| contract("staged file key is outside its snapshot"))?;
        let mut bytes = Vec::new();
        (self.read)(
            SealedGenerationSegmentReadV1::Whole {
                digest: &descriptor.segment_digest,
                size_bytes: descriptor.segment_size_bytes,
            },
            &mut bytes,
        )?;
        let mut restored = Vec::new();
        let page = decode_file_segment(
            descriptor,
            &self.manifest.generation_id,
            &self.manifest.snapshot_digest,
            &self.scope,
            &bytes,
            &mut restored,
        )?;
        let file = restore_file_pages(vec![page])?
            .pop()
            .ok_or_else(|| contract("staged file segment restored no file"))?;
        if file.authority.logical_path != snapshot_file.logical_path
            || file.authority.content_digest != snapshot_file.content_digest
            || snapshot_file
                .language
                .as_ref()
                .map(|language| language.as_str())
                != Some(file.extraction.language.as_str())
        {
            return Err(contract(
                "staged file segment disagrees with its source snapshot",
            ));
        }
        Ok(file)
    }
}

/// A parsed sealed parent.
pub(super) struct SealedParentGenerationV1 {
    sealed: CodeIndexSealedGenerationV1,
    generation: PartitionedPublishedGenerationV1,
    scope: FileScopeIdentityV1,
    segments_by_path: HashMap<String, usize>,
    evidence_by_key: HashMap<u32, usize>,
}

impl SealedParentGenerationV1 {
    pub(super) fn open(
        sealed: CodeIndexSealedGenerationV1,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let generation = parse_partitioned_manifest(&sealed.manifest_bytes)?;
        let scope = FileScopeIdentityV1::of(&generation.manifest, &generation.snapshot);
        let mut segments_by_path = HashMap::with_capacity(generation.file_segments.len());
        for (position, descriptor) in generation.file_segments.iter().enumerate() {
            let file = generation
                .snapshot
                .files
                .get(descriptor.file_key as usize)
                .ok_or_else(|| contract("sealed file segment is outside its snapshot"))?;
            segments_by_path.insert(file.logical_path.clone(), position);
        }
        let evidence_by_key = generation
            .file_evidence
            .iter()
            .enumerate()
            .map(|(position, descriptor)| (descriptor.file_key, position))
            .collect();
        Ok(Self {
            sealed,
            generation,
            scope,
            segments_by_path,
            evidence_by_key,
        })
    }

    pub(super) fn sealed(&self) -> &CodeIndexSealedGenerationV1 {
        &self.sealed
    }

    pub(super) fn manifest(&self) -> &CodeGenerationManifestV1 {
        &self.generation.manifest
    }

    pub(super) fn snapshot(&self) -> &SanitizedCodeSnapshotV1 {
        &self.generation.snapshot
    }

    pub(super) fn statistics(&self) -> &CodeIndexGenerationStatisticsV1 {
        &self.generation.statistics
    }

    pub(super) fn chunk_count(&self) -> u64 {
        self.generation.chunk_count
    }

    pub(super) fn chunk_policy(&self) -> &ChunkPolicyRevisionSummaryV1 {
        &self.generation.chunk_policy
    }

    pub(super) fn coverage(&self) -> tracedecay_domain::CoverageSummaryV1 {
        self.generation.coverage
    }

    pub(super) fn generation(&self) -> &PartitionedPublishedGenerationV1 {
        &self.generation
    }

    pub(super) fn read(&self) -> &SealedSegmentReaderV1 {
        self.sealed.read_segment.as_ref()
    }

    pub(super) fn file_segment(&self, path: &str) -> Option<&PartitionedFileSegmentDescriptorV1> {
        self.segments_by_path
            .get(path)
            .map(|position| &self.generation.file_segments[*position])
    }

    pub(super) fn file_evidence_descriptor(
        &self,
        file_key: u32,
    ) -> Option<&PartitionedFileEvidenceDescriptorV1> {
        self.evidence_by_key
            .get(&file_key)
            .map(|position| &self.generation.file_evidence[*position])
    }

    pub(super) fn graph_pages_by_path(
        &self,
    ) -> BTreeMap<&str, &PartitionedCodeGraphPageDescriptorV1> {
        self.generation
            .code_graph_pages
            .iter()
            .map(|page| (page.logical_path.as_str(), page))
            .collect()
    }

    /// Decode one of the parent's files as the generation `generation_id`
    /// over the snapshot `snapshot_digest` restores it.
    pub(super) fn decode_file(
        &self,
        descriptor: &PartitionedFileSegmentDescriptorV1,
        file_occurrence_id: &FileOccurrenceId,
        generation_id: &CodeGenerationId,
        snapshot_digest: &ManifestDigest,
    ) -> Result<Arc<FileGenerationArtifactsV1>, CodeIndexProductionErrorV1> {
        let mut bytes = Vec::new();
        self.read()(
            SealedGenerationSegmentReadV1::Whole {
                digest: &descriptor.segment_digest,
                size_bytes: descriptor.segment_size_bytes,
            },
            &mut bytes,
        )?;
        let mut rebound = descriptor.clone();
        rebound.file_occurrence_id = file_occurrence_id.clone();
        let mut restored = Vec::new();
        let page = decode_file_segment(
            &rebound,
            generation_id,
            snapshot_digest,
            &self.scope,
            &bytes,
            &mut restored,
        )?;
        restore_file_pages(vec![page])?
            .pop()
            .ok_or_else(|| contract("sealed file segment restored no file"))
    }

    /// Decode the parent's own file at `path` as the parent sealed it.
    pub(super) fn decode_parent_file(
        &self,
        path: &str,
    ) -> Result<Arc<FileGenerationArtifactsV1>, CodeIndexProductionErrorV1> {
        let descriptor = self
            .file_segment(path)
            .ok_or_else(|| contract("sealed parent has no file at the edited path"))?;
        self.decode_file(
            descriptor,
            &descriptor.file_occurrence_id,
            &self.generation.manifest.generation_id,
            &self.generation.manifest.snapshot_digest,
        )
    }

    /// Carry an unchanged present file from the sealed parent into a
    /// successor that must build cold.
    ///
    /// Content and language equality are the reuse authority: a path whose
    /// content or parser changed is not carried, so omitted bytes still fail
    /// closed with [`super::CodeIndexInputErrorV1::MissingCapturedFile`].
    pub(super) fn carry_unchanged_file(
        &self,
        file: &SanitizedCodeFileV1,
        generation_id: &CodeGenerationId,
        snapshot_digest: &ManifestDigest,
    ) -> Result<Option<Arc<FileGenerationArtifactsV1>>, CodeIndexProductionErrorV1> {
        let Some(parent_file) = self
            .snapshot()
            .files
            .iter()
            .find(|parent| parent.logical_path == file.logical_path)
        else {
            return Ok(None);
        };
        if parent_file.content_digest != file.content_digest
            || parent_file.disposition != file.disposition
            || parent_file.language != file.language
        {
            return Ok(None);
        }
        let Some(descriptor) = self.file_segment(&file.logical_path) else {
            return Ok(None);
        };
        self.decode_file(
            descriptor,
            &file.file_occurrence_id,
            generation_id,
            snapshot_digest,
        )
        .map(Some)
    }

    pub(super) fn file_evidence(
        &self,
        file_key: u32,
    ) -> Result<Option<PersistedFileEvidenceV1>, CodeIndexProductionErrorV1> {
        let Some(descriptor) = self.file_evidence_descriptor(file_key) else {
            return Ok(None);
        };
        let mut bytes = Vec::new();
        self.read()(
            SealedGenerationSegmentReadV1::Whole {
                digest: &descriptor.segment_digest,
                size_bytes: descriptor.segment_size_bytes,
            },
            &mut bytes,
        )?;
        decode_file_evidence_segment(descriptor, &bytes).map(Some)
    }

    pub(super) fn generation_evidence(
        &self,
    ) -> Result<PartitionedGenerationEvidenceV1, CodeIndexProductionErrorV1> {
        let read = self.read();
        decode_generation_evidence(&self.generation.generation_evidence, |request, buffer| {
            read(request, buffer)
        })
    }

    pub(super) fn resolution_index(
        &self,
    ) -> Result<ResolutionIndexReaderV1<'_>, CodeIndexProductionErrorV1> {
        ResolutionIndexReaderV1::new(&self.generation.resolution_index, self.read())
    }
}

fn contract(message: &str) -> CodeIndexProductionErrorV1 {
    CodeIndexProductionErrorV1::Contract(message.to_owned())
}

/// One successor file retains only binding facts for this resolution pass.
/// Its parent bytes are read fallibly at the first operation that needs them.
pub(super) struct SparseFileV1<'p> {
    logical_path: &'p str,
    language: &'p str,
    facts: OnceLock<ChargedResolutionFactsV1>,
    source: Option<SparseFileSourceV1<'p>>,
}

struct ChargedResolutionFactsV1 {
    facts: CompactFileResolutionV1,
    _memory: super::ConstructionMemoryV1,
}

pub(super) struct SparseFileSourceV1<'p> {
    pub(super) parent: &'p SealedParentGenerationV1,
    pub(super) descriptor: &'p PartitionedFileSegmentDescriptorV1,
    pub(super) file_occurrence_id: &'p FileOccurrenceId,
    pub(super) generation_id: &'p CodeGenerationId,
    pub(super) snapshot_digest: &'p ManifestDigest,
    pub(super) construction_operation: ResidentMemoryOperationV1,
}

impl<'p> SparseFileV1<'p> {
    pub(super) fn decoded(
        logical_path: &'p str,
        language: &'p str,
        artifacts: Arc<FileGenerationArtifactsV1>,
        construction_operation: ResidentMemoryOperationV1,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let mut memory = super::ConstructionMemoryV1::new(construction_operation)?;
        let facts =
            CompactFileResolutionV1::from_file(&artifacts, &mut |bytes| memory.resize(bytes))?;
        drop(artifacts);
        // Edited artifacts own these shared symbol records throughout the
        // resolution; this view owns only its handles and copied rows.
        let bytes = facts
            .retained_bytes()
            .saturating_sub(facts.owned_symbol_bytes());
        let bytes = u64::try_from(bytes).map_err(|_| contract("resolution facts exceed u64"))?;
        if bytes > memory.charged_bytes {
            return Err(contract(
                "resolution facts exceed their admitted clone bound",
            ));
        }
        memory.resize(bytes)?;
        Ok(Self {
            logical_path,
            language,
            facts: OnceLock::from(ChargedResolutionFactsV1 {
                facts,
                _memory: memory,
            }),
            source: None,
        })
    }

    pub(super) fn carried(
        logical_path: &'p str,
        language: &'p str,
        source: SparseFileSourceV1<'p>,
    ) -> Self {
        Self {
            logical_path,
            language,
            facts: OnceLock::new(),
            source: Some(source),
        }
    }

    pub(super) fn loaded(&self) -> Option<&CompactFileResolutionV1> {
        self.facts.get().map(|charged| &charged.facts)
    }
}

impl ResolutionFileV1 for SparseFileV1<'_> {
    fn logical_path(&self) -> &str {
        self.logical_path
    }
    fn language(&self) -> &str {
        self.language
    }
    fn binding(&self) -> Result<&dyn ResolutionFileFactsV1, CodeIndexProductionErrorV1> {
        if let Some(facts) = self.facts.get() {
            return Ok(&facts.facts);
        }
        let source = self
            .source
            .as_ref()
            .ok_or_else(|| contract("a successor file has no binding source"))?;
        let file = source.parent.decode_file(
            source.descriptor,
            source.file_occurrence_id,
            source.generation_id,
            source.snapshot_digest,
        )?;
        let source_bytes = u64::try_from(resolution_source_bytes(&file)?)
            .map_err(|_| contract("resolution source exceeds u64"))?;
        let mut memory = super::ConstructionMemoryV1::new(source.construction_operation.clone())?;
        let facts = CompactFileResolutionV1::from_file(&file, &mut |copies| {
            let overlap = source_bytes
                .checked_add(copies)
                .ok_or_else(|| contract("resolution source/clone overlap exceeds u64"))?;
            memory.resize(overlap)
        })?;
        drop(file);
        let retained = u64::try_from(facts.retained_bytes())
            .map_err(|_| contract("resolution facts exceed u64"))?;
        if retained > memory.charged_bytes {
            return Err(contract(
                "resolution facts exceed their admitted source/clone bound",
            ));
        }
        memory.resize(retained)?;
        // Concurrent reads can prepare the same immutable facts; only the
        // winning compact value remains and every full decode drops here.
        let _ = self.facts.set(ChargedResolutionFactsV1 {
            facts,
            _memory: memory,
        });
        self.facts
            .get()
            .map(|charged| &charged.facts as &dyn ResolutionFileFactsV1)
            .ok_or_else(|| contract("a successor file did not retain its binding facts"))
    }
}
