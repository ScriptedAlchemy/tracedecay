//! A sealed generation read as the parent of a sparse successor.
//!
//! The parent is its authenticated partitioned manifest and a reader over
//! its content-addressed segments. Nothing is decoded until the successor
//! asks for one file, one file's evidence, one graph page, or one resolution
//! index page, so what a successor build holds is proportional to what it
//! reads.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use tracedecay_domain::{
    CodeGenerationId, CodeGenerationManifestV1, FileOccurrenceId, ManifestDigest,
    SanitizedCodeSnapshotV1,
};

use super::file_evidence_rows::PersistedFileEvidenceV1;
use super::partitioned_codec::{
    PartitionedCodeGraphPageDescriptorV1, PartitionedFileEvidenceDescriptorV1,
    PartitionedFileSegmentDescriptorV1, PartitionedGenerationEvidenceV1,
    PartitionedPublishedGenerationV1, SealedGenerationSegmentReadV1, decode_file_evidence_segment,
    decode_file_segment, decode_generation_evidence, parse_partitioned_manifest,
};
use super::resolution_index::ResolutionIndexReaderV1;
use super::resolution_view::ResolutionFileV1;
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

    /// The authenticated manifest, snapshot, and statistics this generation
    /// seals, parsed without decoding any segment.
    pub fn metadata(
        &self,
    ) -> Result<VerifiedSealedTextGenerationMetadataV1, CodeIndexProductionErrorV1> {
        CodeIndexPublishedGenerationV1::partitioned_text_metadata(&self.manifest_bytes)
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

/// The first decode failure a lazy view recorded.
#[derive(Default)]
pub(super) struct DecodeFailureV1(Mutex<Option<CodeIndexProductionErrorV1>>);

impl DecodeFailureV1 {
    pub(super) fn record(&self, error: CodeIndexProductionErrorV1) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert(error);
    }

    pub(super) fn take(&self) -> Option<CodeIndexProductionErrorV1> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

/// One file of a successor generation as resolution reads it: its path and
/// language from the successor's snapshot, and its artifacts either already
/// decoded or decoded from the parent's carried segment the first time
/// resolution reads them.
pub(super) struct SparseFileV1<'p> {
    logical_path: &'p str,
    language: &'p str,
    artifacts: SparseArtifactsV1<'p>,
}

enum SparseArtifactsV1<'p> {
    Decoded(Arc<FileGenerationArtifactsV1>),
    Carried {
        decoded: OnceLock<Arc<FileGenerationArtifactsV1>>,
        source: SparseFileSourceV1<'p>,
    },
}

/// Where a carried file's artifacts decode from, and what stands in when
/// that decode fails.
pub(super) struct SparseFileSourceV1<'p> {
    pub(super) parent: &'p SealedParentGenerationV1,
    pub(super) descriptor: &'p PartitionedFileSegmentDescriptorV1,
    pub(super) file_occurrence_id: &'p FileOccurrenceId,
    pub(super) generation_id: &'p CodeGenerationId,
    pub(super) snapshot_digest: &'p ManifestDigest,
    /// An already decoded file a failed decode is read as. The failure is
    /// recorded in `failure` and the build refuses before it publishes, so
    /// nothing derived from the stand-in is sealed.
    pub(super) stand_in: &'p Arc<FileGenerationArtifactsV1>,
    pub(super) failure: &'p DecodeFailureV1,
}

impl<'p> SparseFileV1<'p> {
    pub(super) fn decoded(
        logical_path: &'p str,
        language: &'p str,
        artifacts: Arc<FileGenerationArtifactsV1>,
    ) -> Self {
        Self {
            logical_path,
            language,
            artifacts: SparseArtifactsV1::Decoded(artifacts),
        }
    }

    pub(super) fn carried(
        logical_path: &'p str,
        language: &'p str,
        source: SparseFileSourceV1<'p>,
    ) -> Self {
        Self {
            logical_path,
            language,
            artifacts: SparseArtifactsV1::Carried {
                decoded: OnceLock::new(),
                source,
            },
        }
    }

    /// The artifacts, when they are decoded already.
    pub(super) fn loaded(&self) -> Option<&Arc<FileGenerationArtifactsV1>> {
        match &self.artifacts {
            SparseArtifactsV1::Decoded(artifacts) => Some(artifacts),
            SparseArtifactsV1::Carried { decoded, .. } => decoded.get(),
        }
    }

    pub(super) fn artifacts(&self) -> &Arc<FileGenerationArtifactsV1> {
        match &self.artifacts {
            SparseArtifactsV1::Decoded(artifacts) => artifacts,
            SparseArtifactsV1::Carried { decoded, source } => decoded.get_or_init(|| match source
                .parent
                .decode_file(
                    source.descriptor,
                    source.file_occurrence_id,
                    source.generation_id,
                    source.snapshot_digest,
                ) {
                Ok(file) => file,
                Err(error) => {
                    source.failure.record(error);
                    Arc::clone(source.stand_in)
                }
            }),
        }
    }
}

impl AsRef<FileGenerationArtifactsV1> for SparseFileV1<'_> {
    fn as_ref(&self) -> &FileGenerationArtifactsV1 {
        self.artifacts()
    }
}

impl ResolutionFileV1 for SparseFileV1<'_> {
    fn logical_path(&self) -> &str {
        self.logical_path
    }

    fn language(&self) -> &str {
        self.language
    }
}
