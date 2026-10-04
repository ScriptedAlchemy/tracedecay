//! The partitioned generation codec.
//!
//! # Canonical byte rules
//!
//! File-segment payloads are compact serializations transformed by the
//! streaming writer in [`super::canonical_json`]. Generation evidence is one
//! deterministic typed JSON stream split into bounded authenticated pages in
//! one content-addressed pack. Its bytes keep serde's declaration order and
//! original identities; the ordered page descriptors authenticate both each
//! range and the complete stream without materializing it.
//!
//! 1. **Object keys are sorted.** This crate does not enable
//!    `serde_json/preserve_order`, so `serde_json::Map` is a `BTreeMap` and
//!    every object inside a payload is emitted in byte-sorted key order, not
//!    in Rust field-declaration order.
//! 2. **The file segment envelope is declaration ordered.** Only the payload
//!    went through a `Value`; the enclosing record is still
//!    `{"format_revision":<u32>,"file":<payload>}`.
//! 3. **File identity strings are substituted in place** by the key that encloses
//!    them (see [`identity_field`]); the
//!    classification is reset at every object member and inherited through
//!    arrays.
//! 4. **`artifacts.symbols` is in symbol identity order**, serialized that
//!    way, which is also the order of the segment's symbol keys.
//! 5. **`artifacts.edges` and `artifacts.unresolved_references` are sorted by
//!    each element's own canonical encoding**, byte-wise, the shipped
//!    comparator was `sort_by_cached_key(Value::to_string)`.
//! 6. Generation evidence is not rewritten. Its page boundaries do not alter
//!    the typed JSON stream, and an aggregate digest authenticates the exact
//!    concatenation.
//!
//! Decoding needs neither rule 1 nor rules 4-6: `serde` accepts any member
//! order and the typed artifacts are re-sorted after restore, so a segment is
//! restored by substituting identities back into the stored bytes and
//! deserializing them directly.
//!
//! Each file that owns cross-file evidence also has a file evidence segment
//! ([`super::file_evidence_rows`]): its typed JSON, raw DEFLATE compressed,
//! with no identity substitution because it names nothing generation-local.

#[cfg(test)]
use std::collections::BTreeMap;
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::io::{Read, Write as IoWrite};
use std::sync::{Mutex, PoisonError};

use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use tracedecay_domain::{
    FileOccurrenceId, ManifestDigest, SymbolIdentityDigest, SymbolOccurrenceId,
};

use super::canonical_json::{
    CanonicalArrayOrderV1, CanonicalPolicyV1, canonicalize_json_into, visit_json_strings,
    write_json_string,
};
use super::file_evidence_rows::{
    FileEvidenceV1, PersistedFileEvidenceV1, compact_file_evidence, identity_lineage,
};
use super::lexical_page_source::{LEXICAL_FILE_PREFETCH_BYTES_V1, checkpoint};
use super::projection_rows::{
    FileChunkRostersV1, PersistedBatchReceiptRefV1, PersistedBatchReceiptV1,
    PersistedProjectionRequestRefV1, PersistedProjectionRequestV1,
};
use super::resolution_index::{PartitionedResolutionIndexDescriptorV1, seal_resolution_index};
use super::sealed_codec::{
    DecodePeakProbeV1, FileScopeIdentityV1, PersistedFileGenerationArtifactsRefV2,
    PersistedFileGenerationArtifactsV1, PersistedFileGenerationArtifactsV2,
    SEALED_GENERATION_FORMAT_REVISION_V1, StreamingPersistedPublishedGenerationV1,
    assemble_published_generation, restore_file_pages, superseded_sealed_generation_revision,
};
use super::*;

/// The row form described on [`PersistedFileGenerationArtifactsRefV2`],
/// stored as a raw DEFLATE stream of its canonical JSON. Only generations
/// of the current manifest revision address segments, so earlier segment
/// revisions are never read.
const FILE_SEGMENT_FORMAT_REVISION: u32 = 6;
/// The zlib default. Changing it changes stored bytes and therefore every
/// segment's content address, which only costs one generation's reuse.
const FILE_SEGMENT_COMPRESSION_LEVEL: u32 = 6;
const GENERATION_ID_MARKER: &str = "$tracedecay:g";
const SNAPSHOT_DIGEST_MARKER: &str = "$tracedecay:snapshot";
const FILE_OCCURRENCE_ID_MARKER: &str = "$tracedecay:f";
const SYMBOL_OCCURRENCE_ID_MARKER_PREFIX: &str = "$tracedecay:s:";
const GENERATION_EVIDENCE_PAGE_MAX_BYTES_V1: usize = 256 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PartitionedFileSegmentDescriptorV1 {
    pub(super) file_key: u32,
    pub(super) segment_digest: ManifestDigest,
    pub(super) segment_size_bytes: u64,
    /// Length of the canonical JSON the stored bytes inflate to. Restore
    /// windows budget on it, and inflation must end at exactly this length.
    pub(super) decoded_size_bytes: u64,
    pub(super) file_occurrence_id: FileOccurrenceId,
    /// Digest of the ordered symbol identities the segment carries, so a
    /// successor can decide reuse without reading the segment.
    pub(super) symbol_identities_digest: ManifestDigest,
}

fn symbol_identities_digest(
    identities: &[SymbolIdentityDigest],
) -> Result<ManifestDigest, CodeIndexProductionErrorV1> {
    let mut hasher = Sha256::new();
    for identity in identities {
        hasher.update(identity.as_str().as_bytes());
        hasher.update(b"\n");
    }
    ManifestDigest::from_sha256_bytes(&hasher.finalize())
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
}

/// A segment's symbol keys resolve to these occurrences, rebound to the
/// file occurrence of the generation that addresses it.
fn bind_symbol_occurrences(
    file_occurrence_id: &FileOccurrenceId,
    identities: &[SymbolIdentityDigest],
) -> Result<Vec<SymbolOccurrenceId>, CodeIndexProductionErrorV1> {
    identities
        .iter()
        .map(|identity| crate::chunks::symbol_occurrence_id(file_occurrence_id, identity))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PartitionedFileEvidenceDescriptorV1 {
    pub(super) file_key: u32,
    pub(super) file_occurrence_id: FileOccurrenceId,
    pub(super) segment_digest: ManifestDigest,
    pub(super) segment_size_bytes: u64,
    pub(super) decoded_size_bytes: u64,
    /// Whether the segment carries the file's lineage rows, so a successor
    /// that carries the file knows to reseal it with identity lineage.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(super) explicit_lineage: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PartitionedEvidencePageDescriptorV1 {
    page_ordinal: u32,
    page_digest: ManifestDigest,
    page_size_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PartitionedGenerationEvidenceDescriptorV1 {
    segment_digest: ManifestDigest,
    segment_size_bytes: u64,
    /// Prefix containing the typed generation evidence JSON.
    evidence_size_bytes: u64,
    pages: Vec<PartitionedEvidencePageDescriptorV1>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PartitionedCodeGraphPageDescriptorV1 {
    pub(crate) file_key: u32,
    pub(crate) file_occurrence_id: FileOccurrenceId,
    pub(crate) logical_path: String,
    pub(crate) page_digest: ManifestDigest,
    pub(crate) size_bytes: u64,
    pub(crate) build_footprint: super::graph_page_store::CodeGraphPageBuildFootprintV1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedGenerationSegmentIdentityV1 {
    pub digest: ManifestDigest,
    pub size_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SealedGenerationSegmentPublicationV1<'a> {
    File {
        digest: &'a ManifestDigest,
        bytes: &'a [u8],
    },
    /// One file's cross-file evidence segment.
    FileEvidence {
        digest: &'a ManifestDigest,
        bytes: &'a [u8],
    },
    /// One page of the generation's resolution index.
    ResolutionIndex {
        digest: &'a ManifestDigest,
        bytes: &'a [u8],
    },
    CodeGraphPage {
        file_key: u32,
        page_digest: &'a ManifestDigest,
        bytes: &'a [u8],
    },
    GenerationEvidencePage {
        page_ordinal: u32,
        page_digest: &'a ManifestDigest,
        bytes: &'a [u8],
    },
    GenerationEvidenceCommit {
        segment_digest: &'a ManifestDigest,
        segment_size_bytes: u64,
        page_count: u32,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SealedGenerationSegmentReadV1<'a> {
    Whole {
        digest: &'a ManifestDigest,
        size_bytes: u64,
    },
    Range {
        digest: &'a ManifestDigest,
        size_bytes: u64,
        offset: u64,
        length: u64,
    },
}

#[derive(Serialize)]
pub(super) struct PartitionedPublishedGenerationRefV1<'a> {
    pub(super) format_revision: u32,
    pub(super) manifest: &'a CodeGenerationManifestV1,
    pub(super) snapshot: &'a SanitizedCodeSnapshotV1,
    pub(super) statistics: &'a CodeIndexGenerationStatisticsV1,
    pub(super) chunk_count: u64,
    pub(super) chunk_policy: &'a ChunkPolicyRevisionSummaryV1,
    pub(super) repository_parse_identity: &'a CodeIndexRepositoryParseIdentityV1,
    pub(super) ignored_source_admissions: &'a [CodeIndexIgnoredSourceAdmissionV1],
    pub(super) ignored_source_admissions_digest: &'a ManifestDigest,
    pub(super) file_segments: &'a [PartitionedFileSegmentDescriptorV1],
    pub(super) file_evidence: &'a [PartitionedFileEvidenceDescriptorV1],
    pub(super) coverage: CoverageSummaryV1,
    pub(super) capability: &'a CodeIndexCapabilityManifestV1,
    pub(super) generation_evidence: &'a PartitionedGenerationEvidenceDescriptorV1,
    pub(super) code_graph_pages: &'a [PartitionedCodeGraphPageDescriptorV1],
    pub(super) resolution_index: &'a PartitionedResolutionIndexDescriptorV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PartitionedPublishedGenerationV1 {
    /// Gated by the revision probe before this strict parse runs.
    #[serde(rename = "format_revision")]
    _format_revision: u32,
    pub(super) manifest: CodeGenerationManifestV1,
    pub(super) snapshot: SanitizedCodeSnapshotV1,
    pub(super) statistics: CodeIndexGenerationStatisticsV1,
    pub(super) chunk_count: u64,
    pub(super) chunk_policy: ChunkPolicyRevisionSummaryV1,
    pub(super) repository_parse_identity: CodeIndexRepositoryParseIdentityV1,
    pub(super) ignored_source_admissions: Vec<CodeIndexIgnoredSourceAdmissionV1>,
    pub(super) ignored_source_admissions_digest: ManifestDigest,
    pub(super) file_segments: Vec<PartitionedFileSegmentDescriptorV1>,
    pub(super) file_evidence: Vec<PartitionedFileEvidenceDescriptorV1>,
    pub(super) coverage: CoverageSummaryV1,
    pub(super) capability: CodeIndexCapabilityManifestV1,
    pub(super) generation_evidence: PartitionedGenerationEvidenceDescriptorV1,
    pub(super) code_graph_pages: Vec<PartitionedCodeGraphPageDescriptorV1>,
    pub(super) resolution_index: PartitionedResolutionIndexDescriptorV1,
}

#[derive(Serialize)]
struct PartitionedEnvelopeRefV1<'a> {
    state_digest: &'a ManifestDigest,
    generation: &'a RawValue,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PartitionedRawEnvelopeV1<'a> {
    state_digest: ManifestDigest,
    #[serde(borrow)]
    generation: &'a RawValue,
}

#[derive(Deserialize)]
struct PartitionedFormatProbeV1 {
    format_revision: u32,
}

/// Minimal streaming projection used by retention after the generation file's
/// outer content address has already been verified. It retains every field
/// needed to prove that the segment list is complete and canonically keyed,
/// while omitting snapshot bodies and symbol marker maps.
#[derive(Deserialize)]
struct PartitionedSegmentIdentityEnvelopeV1 {
    #[serde(rename = "state_digest")]
    _state_digest: ManifestDigest,
    generation: PartitionedSegmentIdentityGenerationV1,
}

#[derive(Deserialize)]
struct PartitionedSegmentIdentityGenerationV1 {
    format_revision: u32,
    snapshot: PartitionedSegmentIdentitySnapshotV1,
    file_segments: Vec<PartitionedFileSegmentIdentityV1>,
    /// Absent from retired manifests, which reach the revision gate and
    /// abstain; a current one always carries the list.
    #[serde(default)]
    file_evidence: Vec<PartitionedFileEvidenceIdentityV1>,
    generation_evidence: PartitionedEvidenceSegmentIdentityV1,
    #[serde(default)]
    code_graph_pages: Vec<PartitionedCodeGraphPageIdentityV1>,
    /// Absent from retired manifests, which abstain at the revision gate.
    #[serde(default)]
    resolution_index: Option<PartitionedResolutionIndexDescriptorV1>,
}

#[derive(Deserialize)]
struct PartitionedSegmentIdentitySnapshotV1 {
    files: Vec<PartitionedSnapshotFileIdentityV1>,
}

#[derive(Deserialize)]
struct PartitionedSnapshotFileIdentityV1 {
    file_occurrence_id: FileOccurrenceId,
    logical_path: String,
    disposition: SnapshotFileDispositionV1,
}

#[derive(Deserialize)]
struct PartitionedFileSegmentIdentityV1 {
    file_key: u32,
    segment_digest: ManifestDigest,
    segment_size_bytes: u64,
    file_occurrence_id: FileOccurrenceId,
}

#[derive(Deserialize)]
struct PartitionedFileEvidenceIdentityV1 {
    file_key: u32,
    file_occurrence_id: FileOccurrenceId,
    segment_digest: ManifestDigest,
    segment_size_bytes: u64,
}

/// Retention projects the descriptor before its revision gate, so a retired
/// manifest without a page table must still reach that gate and abstain; a
/// current-revision manifest without one fails the shared layout validator.
#[derive(Deserialize)]
struct PartitionedEvidenceSegmentIdentityV1 {
    segment_digest: ManifestDigest,
    segment_size_bytes: u64,
    #[serde(default)]
    pages: Vec<PartitionedEvidencePageIdentityV1>,
}

#[derive(Deserialize)]
struct PartitionedEvidencePageIdentityV1 {
    page_ordinal: u32,
    #[serde(rename = "page_digest")]
    _page_digest: ManifestDigest,
    page_size_bytes: u64,
}

#[derive(Deserialize)]
struct PartitionedCodeGraphPageIdentityV1 {
    file_key: u32,
    file_occurrence_id: FileOccurrenceId,
    logical_path: String,
    page_digest: ManifestDigest,
    size_bytes: u64,
}

/// The stored file segment envelope. Encoding writes the two fields directly
/// (rule 2) and decoding borrows the payload without parsing it into a tree.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PartitionedRawFileSegmentV1<'a> {
    format_revision: u32,
    /// The identities the payload's symbol keys index, in key order.
    symbol_identities: Vec<SymbolIdentityDigest>,
    #[serde(borrow)]
    file: &'a RawValue,
}

/// The evidence stream: the projection request and receipt, whose persisted
/// rows ([`super::projection_rows`]) index the generation's own chunks, so
/// restore expands them only after the file segments supply that roster, and
/// the prior generation every file's implicit lineage rows continue from.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PartitionedGenerationEvidenceV1 {
    #[serde(default)]
    pub(super) lineage_prior_generation: Option<CodeGenerationId>,
    #[serde(deserialize_with = "deserialize_evidence_projection_request")]
    pub(super) projection_request: PersistedProjectionRequestV1,
    #[serde(deserialize_with = "deserialize_evidence_projection_receipt")]
    pub(super) projection_receipt: PersistedBatchReceiptV1,
}

/// The generation evidence stream decodes on one thread, so its request and
/// receipt payloads are measured separately.
fn deserialize_evidence_projection_request<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<PersistedProjectionRequestV1, D::Error> {
    {
        let _span =
            tracing::trace_span!("code_index.restore.evidence_projection_request").entered();
        Deserialize::deserialize(deserializer)
    }
}

fn deserialize_evidence_projection_receipt<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<PersistedBatchReceiptV1, D::Error> {
    {
        let _span =
            tracing::trace_span!("code_index.restore.evidence_projection_receipt").entered();
        Deserialize::deserialize(deserializer)
    }
}

#[derive(Serialize)]
pub(super) struct PartitionedGenerationEvidenceRefV1<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    lineage_prior_generation: Option<&'a CodeGenerationId>,
    projection_request: PersistedProjectionRequestRefV1<'a>,
    projection_receipt: PersistedBatchReceiptRefV1<'a>,
}

impl<'a> PartitionedGenerationEvidenceRefV1<'a> {
    /// `rosters` holds every file whose chunks `projection` adds or changes.
    pub(super) fn new(
        projection: &'a ProjectionPublicationHandoffV1,
        rosters: &FileChunkRostersV1<'_>,
        lineage_prior_generation: Option<&'a CodeGenerationId>,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let request = projection.request();
        Ok(Self {
            lineage_prior_generation,
            projection_request: PersistedProjectionRequestRefV1::new(request, rosters)?,
            projection_receipt: PersistedBatchReceiptRefV1::new(request, projection.receipt())?,
        })
    }
}

#[derive(Clone, Copy)]
enum IdentityFieldV1 {
    Other,
    Generation,
    SnapshotDigest,
    FileOccurrence,
    SymbolOccurrence,
}

fn identity_field(key: &str) -> IdentityFieldV1 {
    match key {
        "generation_id" | "source_generation" => IdentityFieldV1::Generation,
        "snapshot_digest" => IdentityFieldV1::SnapshotDigest,
        "file_occurrence_id" => IdentityFieldV1::FileOccurrence,
        "occurrence"
        | "from_occurrence"
        | "to_occurrence"
        | "prior_occurrence"
        | "current_occurrence"
        | "alternatives"
        | "symbol_occurrence_id"
        | "symbol_occurrence_ids" => IdentityFieldV1::SymbolOccurrence,
        _ => IdentityFieldV1::Other,
    }
}

/// Substitutes a file segment's identities with their canonical markers while
/// the serialized payload is rewritten (rules 1, 3, 4 and 5).
struct FileSegmentEncodePolicyV1<'a> {
    generation_id: &'a str,
    snapshot_digest: Option<&'a str>,
    file_occurrence_id: &'a str,
    occurrence_identities: &'a HashMap<&'a str, &'a SymbolIdentityDigest>,
    identity_keys: HashMap<&'a str, u32>,
    marker: String,
}

impl CanonicalPolicyV1 for FileSegmentEncodePolicyV1<'_> {
    type Field = IdentityFieldV1;

    fn root_field(&self) -> Self::Field {
        IdentityFieldV1::Other
    }

    fn field_for_key(&self, key: &str) -> Self::Field {
        identity_field(key)
    }

    fn rewrite_string(
        &mut self,
        field: Self::Field,
        value: &str,
        out: &mut Vec<u8>,
    ) -> Result<bool, CodeIndexProductionErrorV1> {
        match field {
            IdentityFieldV1::Generation if value == self.generation_id => {
                write_json_string(GENERATION_ID_MARKER, out)?;
                Ok(true)
            }
            IdentityFieldV1::SnapshotDigest if Some(value) == self.snapshot_digest => {
                write_json_string(SNAPSHOT_DIGEST_MARKER, out)?;
                Ok(true)
            }
            IdentityFieldV1::FileOccurrence if value == self.file_occurrence_id => {
                write_json_string(FILE_OCCURRENCE_ID_MARKER, out)?;
                Ok(true)
            }
            IdentityFieldV1::SymbolOccurrence => {
                let Some(identity) = self.occurrence_identities.get(value) else {
                    return Ok(false);
                };
                let key = self
                    .identity_keys
                    .get(identity.as_str())
                    .copied()
                    .ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed file segment symbol identity has no descriptor key".to_owned(),
                        )
                    })?;
                self.marker.clear();
                self.marker.push_str(SYMBOL_OCCURRENCE_ID_MARKER_PREFIX);
                write!(self.marker, "{key}")
                    .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
                write_json_string(&self.marker, out)?;
                Ok(true)
            }
            IdentityFieldV1::Other
            | IdentityFieldV1::Generation
            | IdentityFieldV1::SnapshotDigest
            | IdentityFieldV1::FileOccurrence => Ok(false),
        }
    }

    fn sorts_object_keys(&self) -> bool {
        true
    }

    fn array_order(&self, path: &[&[u8]]) -> CanonicalArrayOrderV1 {
        if path.len() != 2 || path[0] != b"artifacts".as_slice() {
            return CanonicalArrayOrderV1::AsIs;
        }
        if path[1] == b"edges".as_slice() || path[1] == b"unresolved_references".as_slice() {
            return CanonicalArrayOrderV1::ByEncodedBytes;
        }
        CanonicalArrayOrderV1::AsIs
    }
}

/// Restores a file segment's identities from its canonical markers.
struct FileSegmentDecodePolicyV1<'a> {
    generation_id: &'a str,
    snapshot_digest: &'a str,
    file_occurrence_id: &'a str,
    symbol_occurrences: &'a [SymbolOccurrenceId],
}

impl CanonicalPolicyV1 for FileSegmentDecodePolicyV1<'_> {
    type Field = IdentityFieldV1;

    fn root_field(&self) -> Self::Field {
        IdentityFieldV1::Other
    }

    fn field_for_key(&self, key: &str) -> Self::Field {
        identity_field(key)
    }

    fn rewrite_string(
        &mut self,
        field: Self::Field,
        value: &str,
        out: &mut Vec<u8>,
    ) -> Result<bool, CodeIndexProductionErrorV1> {
        match field {
            IdentityFieldV1::Generation if value == GENERATION_ID_MARKER => {
                write_json_string(self.generation_id, out)?;
                Ok(true)
            }
            IdentityFieldV1::SnapshotDigest if value == SNAPSHOT_DIGEST_MARKER => {
                write_json_string(self.snapshot_digest, out)?;
                Ok(true)
            }
            IdentityFieldV1::FileOccurrence if value == FILE_OCCURRENCE_ID_MARKER => {
                write_json_string(self.file_occurrence_id, out)?;
                Ok(true)
            }
            IdentityFieldV1::SymbolOccurrence
                if value.starts_with(SYMBOL_OCCURRENCE_ID_MARKER_PREFIX) =>
            {
                let occurrence = value
                    .strip_prefix(SYMBOL_OCCURRENCE_ID_MARKER_PREFIX)
                    .and_then(|key| key.parse::<usize>().ok())
                    .and_then(|key| self.symbol_occurrences.get(key))
                    .ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed file segment contains an invalid symbol identity key"
                                .to_owned(),
                        )
                    })?;
                write_json_string(occurrence.as_str(), out)?;
                Ok(true)
            }
            IdentityFieldV1::Other
            | IdentityFieldV1::Generation
            | IdentityFieldV1::SnapshotDigest
            | IdentityFieldV1::FileOccurrence
            | IdentityFieldV1::SymbolOccurrence => Ok(false),
        }
    }

    fn sorts_object_keys(&self) -> bool {
        false
    }
}

struct PartitionedEvidencePageWriterV1<'a, P> {
    publish: &'a mut P,
    page: Vec<u8>,
    descriptors: Vec<PartitionedEvidencePageDescriptorV1>,
    segment_hasher: Sha256,
    segment_size_bytes: u64,
    publish_error: Option<CodeIndexProductionErrorV1>,
    #[cfg(test)]
    peak_retained_owned_bytes: usize,
    #[cfg(test)]
    peak_page_capacity: usize,
}

impl<'a, P> PartitionedEvidencePageWriterV1<'a, P>
where
    P: FnMut(SealedGenerationSegmentPublicationV1<'_>) -> Result<(), CodeIndexProductionErrorV1>,
{
    fn new(publish: &'a mut P) -> Self {
        Self {
            publish,
            page: Vec::with_capacity(GENERATION_EVIDENCE_PAGE_MAX_BYTES_V1),
            descriptors: Vec::new(),
            segment_hasher: Sha256::new(),
            segment_size_bytes: 0,
            publish_error: None,
            #[cfg(test)]
            peak_retained_owned_bytes: GENERATION_EVIDENCE_PAGE_MAX_BYTES_V1,
            #[cfg(test)]
            peak_page_capacity: GENERATION_EVIDENCE_PAGE_MAX_BYTES_V1,
        }
    }

    fn remember_error(&mut self, error: CodeIndexProductionErrorV1) -> std::io::Error {
        self.publish_error = Some(error);
        std::io::Error::other("sealed generation evidence page publication failed")
    }

    fn flush_page(&mut self) -> std::io::Result<()> {
        if self.page.is_empty() {
            return Ok(());
        }
        let page_ordinal = u32::try_from(self.descriptors.len()).map_err(|_| {
            self.remember_error(CodeIndexProductionErrorV1::Contract(
                "sealed generation evidence page count exceeds u32".to_owned(),
            ))
        })?;
        let page_digest =
            ManifestDigest::from_sha256_bytes(&Sha256::digest(&self.page)).map_err(|error| {
                self.remember_error(CodeIndexProductionErrorV1::Contract(error.to_string()))
            })?;
        let page_size_bytes = u64::try_from(self.page.len()).map_err(|_| {
            self.remember_error(CodeIndexProductionErrorV1::Contract(
                "sealed generation evidence page length exceeds u64".to_owned(),
            ))
        })?;
        if let Err(error) = (self.publish)(
            SealedGenerationSegmentPublicationV1::GenerationEvidencePage {
                page_ordinal,
                page_digest: &page_digest,
                bytes: &self.page,
            },
        ) {
            return Err(self.remember_error(error));
        }
        // Serde writes small fragments into the already bounded page. Hash its
        // complete byte stream once here instead of updating SHA for every
        // punctuation mark and string fragment.
        self.segment_hasher.update(&self.page);
        self.descriptors.push(PartitionedEvidencePageDescriptorV1 {
            page_ordinal,
            page_digest,
            page_size_bytes,
        });
        self.page.clear();
        #[cfg(test)]
        self.observe_retained_owned_bytes();
        Ok(())
    }

    fn finish(
        &mut self,
        evidence_size_bytes: u64,
    ) -> Result<PartitionedGenerationEvidenceDescriptorV1, CodeIndexProductionErrorV1> {
        self.flush_page().map_err(|error| {
            self.publish_error.take().unwrap_or_else(|| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation evidence page publication failed: {error}"
                ))
            })
        })?;
        let segment_hasher = std::mem::replace(&mut self.segment_hasher, Sha256::new());
        let segment_digest = ManifestDigest::from_sha256_bytes(&segment_hasher.finalize())
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        let page_count = u32::try_from(self.descriptors.len()).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation evidence page count exceeds u32".to_owned(),
            )
        })?;
        (self.publish)(
            SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit {
                segment_digest: &segment_digest,
                segment_size_bytes: self.segment_size_bytes,
                page_count,
            },
        )?;
        Ok(PartitionedGenerationEvidenceDescriptorV1 {
            segment_digest,
            segment_size_bytes: self.segment_size_bytes,
            evidence_size_bytes,
            pages: std::mem::take(&mut self.descriptors),
        })
    }

    fn position(&self) -> u64 {
        self.segment_size_bytes
    }

    fn take_publish_error(&mut self) -> Option<CodeIndexProductionErrorV1> {
        self.publish_error.take()
    }

    #[cfg(test)]
    fn retained_owned_bytes(&self) -> usize {
        self.page
            .capacity()
            .saturating_add(
                self.descriptors
                    .capacity()
                    .saturating_mul(std::mem::size_of::<PartitionedEvidencePageDescriptorV1>()),
            )
            .saturating_add(
                self.descriptors
                    .iter()
                    .map(|descriptor| descriptor.page_digest.as_str().len())
                    .sum::<usize>(),
            )
    }

    #[cfg(test)]
    fn observe_retained_owned_bytes(&mut self) {
        self.peak_page_capacity = self.peak_page_capacity.max(self.page.capacity());
        self.peak_retained_owned_bytes = self
            .peak_retained_owned_bytes
            .max(self.retained_owned_bytes());
    }
}

impl<P> IoWrite for PartitionedEvidencePageWriterV1<'_, P>
where
    P: FnMut(SealedGenerationSegmentPublicationV1<'_>) -> Result<(), CodeIndexProductionErrorV1>,
{
    fn write(&mut self, mut bytes: &[u8]) -> std::io::Result<usize> {
        let written = bytes.len();
        while !bytes.is_empty() {
            let available = GENERATION_EVIDENCE_PAGE_MAX_BYTES_V1 - self.page.len();
            let consumed = available.min(bytes.len());
            let (head, tail) = bytes.split_at(consumed);
            self.page.extend_from_slice(head);
            self.segment_size_bytes = self
                .segment_size_bytes
                .checked_add(u64::try_from(consumed).map_err(|_| {
                    self.remember_error(CodeIndexProductionErrorV1::Contract(
                        "sealed generation evidence payload length exceeds u64".to_owned(),
                    ))
                })?)
                .ok_or_else(|| {
                    self.remember_error(CodeIndexProductionErrorV1::Contract(
                        "sealed generation evidence payload length exceeds u64".to_owned(),
                    ))
                })?;
            #[cfg(test)]
            self.observe_retained_owned_bytes();
            bytes = tail;
            if self.page.len() == GENERATION_EVIDENCE_PAGE_MAX_BYTES_V1 {
                self.flush_page()?;
            }
        }
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct PartitionedEvidencePageReaderV1<'a, R> {
    descriptor: &'a PartitionedGenerationEvidenceDescriptorV1,
    read_segment: &'a mut R,
    page: Vec<u8>,
    page_offset: usize,
    next_page: usize,
    /// The aggregate segment digest is never recomputed. The manifest carries
    /// a digest per page and is itself authenticated before one page is
    /// requested, so every byte the stream yields arrives inside a page this
    /// reader already verified; the page table's sizes must sum to the
    /// segment size, and [`Self::finish`] refuses unless every page was read
    /// and drained.
    segment_offset: u64,
    read_error: Option<CodeIndexProductionErrorV1>,
}

impl<'a, R> PartitionedEvidencePageReaderV1<'a, R>
where
    R: FnMut(
        SealedGenerationSegmentReadV1<'_>,
        &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1>,
{
    fn new(
        descriptor: &'a PartitionedGenerationEvidenceDescriptorV1,
        read_segment: &'a mut R,
    ) -> Self {
        Self {
            descriptor,
            read_segment,
            page: Vec::new(),
            page_offset: 0,
            next_page: 0,
            segment_offset: 0,
            read_error: None,
        }
    }

    fn remember_error(&mut self, error: CodeIndexProductionErrorV1) -> std::io::Error {
        self.read_error = Some(error);
        std::io::Error::other("sealed generation evidence page read failed")
    }

    fn load_next_page(&mut self) -> std::io::Result<bool> {
        let Some(descriptor) = self.descriptor.pages.get(self.next_page) else {
            return Ok(false);
        };
        self.page.clear();
        self.page_offset = 0;
        if let Err(error) = (self.read_segment)(
            SealedGenerationSegmentReadV1::Range {
                digest: &self.descriptor.segment_digest,
                size_bytes: self.descriptor.segment_size_bytes,
                offset: self.segment_offset,
                length: descriptor.page_size_bytes,
            },
            &mut self.page,
        ) {
            return Err(self.remember_error(error));
        }
        if let Err(error) = verify_segment_identity(
            &self.page,
            &descriptor.page_digest,
            descriptor.page_size_bytes,
            "sealed generation evidence page length exceeds u64",
            "sealed generation evidence page byte size does not match its manifest",
            "sealed generation evidence page digest does not match its manifest",
        ) {
            return Err(self.remember_error(error));
        }
        self.page_offset = 0;
        self.next_page += 1;
        self.segment_offset = self
            .segment_offset
            .checked_add(descriptor.page_size_bytes)
            .ok_or_else(|| {
                self.remember_error(CodeIndexProductionErrorV1::Contract(
                    "sealed generation evidence segment length exceeds u64".to_owned(),
                ))
            })?;
        Ok(true)
    }

    fn take_read_error(&mut self) -> Option<CodeIndexProductionErrorV1> {
        self.read_error.take()
    }

    fn finish(mut self) -> Result<(), CodeIndexProductionErrorV1> {
        if let Some(error) = self.read_error.take() {
            return Err(error);
        }
        if self.next_page != self.descriptor.pages.len()
            || self.page_offset != self.page.len()
            || self.segment_offset != self.descriptor.segment_size_bytes
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation evidence segment byte size does not match its manifest"
                    .to_owned(),
            ));
        }
        Ok(())
    }
}

impl<R> Read for PartitionedEvidencePageReaderV1<'_, R>
where
    R: FnMut(
        SealedGenerationSegmentReadV1<'_>,
        &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1>,
{
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        if self.read_error.is_some() {
            return Err(std::io::Error::other(
                "sealed generation evidence page read already failed",
            ));
        }
        while self.page_offset == self.page.len() {
            if !self.load_next_page()? {
                return Ok(0);
            }
        }
        let available = &self.page[self.page_offset..];
        let copied = available.len().min(out.len());
        out[..copied].copy_from_slice(&available[..copied]);
        self.page_offset += copied;
        Ok(copied)
    }
}

/// Encoded file segments per worker held in memory ahead of the ordered
/// publish; sized so a window keeps the pool busy without a batch barrier
/// after every file.
const SEALED_ENCODE_WINDOW_FILES_PER_WORKER_V1: usize = 4;

/// One file's sealed-segment outcome from the parallel plan: a parent
/// segment reused unchanged, or fresh bytes awaiting the ordered publish.
enum FileSegmentPlanV1 {
    Reused(PartitionedFileSegmentDescriptorV1),
    Encoded(PartitionedFileSegmentDescriptorV1, Vec<u8>),
}

/// One file segment's encode buffers: the serde staging payload and the
/// canonical segment. Files encode on the indexing pool, taking a cleared
/// pair from `SealedEncodeBufferPoolV1` and handing the `segment` to the
/// publish phase, which returns it once the bytes are durable; the pool is
/// bounded by the encode window, not by file count.
#[derive(Default)]
struct PartitionedSegmentEncoderV1 {
    payload: Vec<u8>,
    segment: Vec<u8>,
}

/// Cleared encode buffers returned by the phase that finished with them.
///
/// A file's staging payload and canonical segment each grow to segment size
/// from empty, so a fresh pair per file allocates a repository-sized stream of
/// transient buffers. The pool holds only what encoding already keeps live,
/// one window of segments plus one payload per worker, and hands the same
/// capacities back, so the growth is paid for the largest file rather than for
/// every file. Buffers are cleared before reuse, so segment bytes and digests
/// are the ones a fresh pair produced.
#[derive(Default)]
struct SealedEncodeBufferPoolV1(Mutex<Vec<Vec<u8>>>);

impl SealedEncodeBufferPoolV1 {
    fn take(&self) -> Vec<u8> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop()
            .unwrap_or_default()
    }

    fn give(&self, mut buffer: Vec<u8>) {
        buffer.clear();
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(buffer);
    }
}

/// Encode one file's segment for the generation `generation_id`, keyed at
/// `file_key` in its snapshot.
pub(super) fn encode_file_segment(
    generation_id: &CodeGenerationId,
    scope: &FileScopeIdentityV1,
    file: &FileGenerationArtifactsV1,
    file_key: u32,
) -> Result<(PartitionedFileSegmentDescriptorV1, Vec<u8>), CodeIndexProductionErrorV1> {
    let mut encoder = PartitionedSegmentEncoderV1::default();
    let descriptor = encoder.encode_file_segment(generation_id, scope, file, file_key)?;
    Ok((descriptor, encoder.segment))
}

impl PartitionedSegmentEncoderV1 {
    #[cfg(test)]
    fn segment_bytes(&self) -> &[u8] {
        &self.segment
    }

    #[tracing::instrument(name = "code_index.sealed_encode.file", level = "trace", skip_all)]
    fn encode_file_segment(
        &mut self,
        generation_id: &CodeGenerationId,
        scope: &FileScopeIdentityV1,
        file: &FileGenerationArtifactsV1,
        file_key: u32,
    ) -> Result<PartitionedFileSegmentDescriptorV1, CodeIndexProductionErrorV1> {
        self.payload.clear();
        serde_json::to_writer(
            &mut self.payload,
            &PersistedFileGenerationArtifactsRefV2::new(
                scope,
                &file.authority,
                &file.extraction,
                &file.artifacts,
            )?,
        )
        .map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed file segment serialization failed: {error}"
            ))
        })?;
        let occurrence_identities = file
            .artifacts
            .symbols
            .iter()
            .map(|symbol| (symbol.occurrence.as_str(), &symbol.identity))
            .collect::<HashMap<_, _>>();
        self.encode_serialized_file_segment(
            FILE_SEGMENT_FORMAT_REVISION,
            generation_id,
            file.artifacts
                .clone_bodies
                .first()
                .map(|body| body.occurrence.snapshot_digest.as_str()),
            file.extraction.file_occurrence_id.clone(),
            file_key,
            &occurrence_identities,
        )
    }

    /// Rewrite the serialization already staged in `payload` into one canonical
    /// segment and compress it into `segment`. The typed and test entry points
    /// share this single authority so production never carries a second
    /// encoder. The content address covers the stored bytes, which the store
    /// and retention verify without inflating them.
    #[tracing::instrument(
        name = "code_index.sealed_encode.file_rewrite",
        level = "trace",
        skip_all
    )]
    fn encode_serialized_file_segment<'s>(
        &mut self,
        format_revision: u32,
        generation_id: &CodeGenerationId,
        snapshot_digest: Option<&str>,
        file_occurrence_id: FileOccurrenceId,
        file_key: u32,
        occurrence_identities: &HashMap<&'s str, &'s SymbolIdentityDigest>,
    ) -> Result<PartitionedFileSegmentDescriptorV1, CodeIndexProductionErrorV1> {
        let Self { payload, segment } = self;
        let mut ordered_identities = BTreeSet::new();
        visit_json_strings(
            payload,
            IdentityFieldV1::Other,
            &identity_field,
            &mut |field, value| {
                if matches!(field, IdentityFieldV1::SymbolOccurrence)
                    && let Some(identity) = occurrence_identities.get(value.as_ref())
                {
                    ordered_identities.insert(*identity);
                }
                Ok(())
            },
        )?;
        let symbol_identities = ordered_identities.into_iter().cloned().collect::<Vec<_>>();
        let identity_keys = symbol_identities
            .iter()
            .enumerate()
            .map(|(key, identity)| {
                u32::try_from(key)
                    .map(|key| (identity.as_str(), key))
                    .map_err(|_| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed file segment symbol key exceeds u32".to_owned(),
                        )
                    })
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        let mut policy = FileSegmentEncodePolicyV1 {
            generation_id: generation_id.as_str(),
            snapshot_digest,
            file_occurrence_id: file_occurrence_id.as_str(),
            occurrence_identities,
            identity_keys,
            marker: String::new(),
        };
        segment.clear();
        segment.extend_from_slice(b"{\"format_revision\":");
        let serialization_failed = |error: serde_json::Error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed file segment serialization failed: {error}"
            ))
        };
        serde_json::to_writer(&mut *segment, &format_revision).map_err(serialization_failed)?;
        segment.extend_from_slice(b",\"symbol_identities\":");
        serde_json::to_writer(&mut *segment, &symbol_identities).map_err(serialization_failed)?;
        segment.extend_from_slice(b",\"file\":");
        canonicalize_json_into(payload, &mut policy, segment)?;
        segment.push(b'}');
        let length = |bytes: &[u8]| {
            u64::try_from(bytes.len()).map_err(|_| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed file segment length exceeds u64".to_owned(),
                )
            })
        };
        let decoded_size_bytes = length(segment)?;
        let compression_failed = |error: std::io::Error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed file segment compression failed: {error}"
            ))
        };
        payload.clear();
        let mut encoder = DeflateEncoder::new(
            std::mem::take(payload),
            Compression::new(FILE_SEGMENT_COMPRESSION_LEVEL),
        );
        encoder.write_all(segment).map_err(compression_failed)?;
        *payload = encoder.finish().map_err(compression_failed)?;
        std::mem::swap(payload, segment);
        let segment_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(&*segment))
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        Ok(PartitionedFileSegmentDescriptorV1 {
            file_key,
            segment_digest,
            segment_size_bytes: length(segment)?,
            decoded_size_bytes,
            file_occurrence_id,
            symbol_identities_digest: symbol_identities_digest(&symbol_identities)?,
        })
    }
}

/// Write the generation evidence stream as bounded pages of one pack.
#[tracing::instrument(name = "code_index.sealed_encode.evidence", level = "trace", skip_all)]
pub(super) fn write_generation_evidence(
    evidence: &PartitionedGenerationEvidenceRefV1<'_>,
    publish: &mut impl FnMut(
        SealedGenerationSegmentPublicationV1<'_>,
    ) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<PartitionedGenerationEvidenceDescriptorV1, CodeIndexProductionErrorV1> {
    let mut writer = PartitionedEvidencePageWriterV1::new(publish);
    let encoded = serde_json::to_writer(&mut writer, evidence);
    if let Some(error) = writer.take_publish_error() {
        return Err(error);
    }
    encoded.map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation evidence serialization failed: {error}"
        ))
    })?;
    let evidence_size_bytes = writer.position();
    writer.finish(evidence_size_bytes)
}

/// Publish one sealed code graph page unless `reusable` already stores its
/// bytes, and return its descriptor.
pub(super) fn publish_code_graph_page(
    page: super::graph_pages::SealedCodeGraphPageV1,
    reusable: &BTreeSet<ManifestDigest>,
    publish: &mut impl FnMut(
        SealedGenerationSegmentPublicationV1<'_>,
    ) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<PartitionedCodeGraphPageDescriptorV1, CodeIndexProductionErrorV1> {
    let size_bytes = u64::try_from(page.encoded.len()).map_err(|_| {
        CodeIndexProductionErrorV1::Contract("sealed code graph page length exceeds u64".to_owned())
    })?;
    if !reusable.contains(&page.page_digest) {
        publish(SealedGenerationSegmentPublicationV1::CodeGraphPage {
            file_key: page.file_key,
            page_digest: &page.page_digest,
            bytes: &page.encoded,
        })?;
    }
    Ok(PartitionedCodeGraphPageDescriptorV1 {
        file_key: page.file_key,
        file_occurrence_id: page.file_occurrence_id,
        logical_path: page.logical_path,
        page_digest: page.page_digest,
        size_bytes,
        build_footprint: page.build_footprint,
    })
}

fn verify_segment_identity(
    bytes: &[u8],
    digest: &ManifestDigest,
    size_bytes: u64,
    length_message: &'static str,
    size_message: &'static str,
    digest_message: &'static str,
) -> Result<(), CodeIndexProductionErrorV1> {
    let actual_size = u64::try_from(bytes.len())
        .map_err(|_| CodeIndexProductionErrorV1::Contract(length_message.to_owned()))?;
    if actual_size != size_bytes {
        return Err(CodeIndexProductionErrorV1::Contract(
            size_message.to_owned(),
        ));
    }
    let actual_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes))
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
    if &actual_digest != digest {
        return Err(CodeIndexProductionErrorV1::Contract(
            digest_message.to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn decode_file_segment(
    descriptor: &PartitionedFileSegmentDescriptorV1,
    generation_id: &CodeGenerationId,
    snapshot_digest: &ManifestDigest,
    scope: &FileScopeIdentityV1,
    bytes: &[u8],
    restored: &mut Vec<u8>,
) -> Result<PersistedFileGenerationArtifactsV1, CodeIndexProductionErrorV1> {
    {
        let _span = tracing::trace_span!("code_index.restore.segment_verify").entered();
        verify_segment_identity(
            bytes,
            &descriptor.segment_digest,
            descriptor.segment_size_bytes,
            "sealed file segment length exceeds u64",
            "sealed file segment byte size does not match its manifest",
            "sealed file segment digest does not match its manifest",
        )
    }?;
    {
        let _span = tracing::trace_span!("code_index.restore.segment_decode").entered();
        decode_verified_file_segment(
            descriptor,
            generation_id,
            snapshot_digest,
            scope,
            bytes,
            restored,
        )
    }
}

/// Inflate stored segment bytes into exactly `decoded_size_bytes` of
/// canonical JSON; the manifest-authenticated length also bounds the output.
fn inflate_file_segment(
    bytes: &[u8],
    decoded_size_bytes: u64,
    out: &mut Vec<u8>,
) -> Result<(), CodeIndexProductionErrorV1> {
    let expected = usize::try_from(decoded_size_bytes).map_err(|_| {
        CodeIndexProductionErrorV1::Contract(
            "sealed file segment decoded size exceeds addressable memory".to_owned(),
        )
    })?;
    out.clear();
    out.try_reserve_exact(expected).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed file segment decoded size cannot be allocated: {error}"
        ))
    })?;
    DeflateDecoder::new(bytes)
        .take(decoded_size_bytes.saturating_add(1))
        .read_to_end(out)
        .map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed file segment does not inflate: {error}"
            ))
        })?;
    if out.len() != expected {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed file segment does not inflate to its manifest size".to_owned(),
        ));
    }
    Ok(())
}

pub(super) fn verify_index_segment(
    bytes: &[u8],
    digest: &ManifestDigest,
    size_bytes: u64,
) -> Result<(), CodeIndexProductionErrorV1> {
    verify_segment_identity(
        bytes,
        digest,
        size_bytes,
        "sealed resolution index length exceeds u64",
        "sealed resolution index byte size does not match its manifest",
        "sealed resolution index digest does not match its manifest",
    )
}

pub(super) fn inflate_index_segment(
    bytes: &[u8],
    decoded_size_bytes: u64,
) -> Result<Vec<u8>, CodeIndexProductionErrorV1> {
    let mut canonical = Vec::new();
    inflate_file_segment(bytes, decoded_size_bytes, &mut canonical)?;
    Ok(canonical)
}

/// One file's evidence segment: its stored bytes and their descriptor.
pub(super) fn encode_file_evidence_segment(
    file_key: u32,
    file_occurrence_id: &FileOccurrenceId,
    evidence: &PersistedFileEvidenceV1,
) -> Result<(PartitionedFileEvidenceDescriptorV1, Vec<u8>), CodeIndexProductionErrorV1> {
    let failed = |error: &dyn std::fmt::Display| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed file evidence encoding failed: {error}"
        ))
    };
    let canonical = serde_json::to_vec(evidence).map_err(|error| failed(&error))?;
    let mut encoder =
        DeflateEncoder::new(Vec::new(), Compression::new(FILE_SEGMENT_COMPRESSION_LEVEL));
    encoder
        .write_all(&canonical)
        .map_err(|error| failed(&error))?;
    let bytes = encoder.finish().map_err(|error| failed(&error))?;
    let length = |bytes: &[u8]| {
        u64::try_from(bytes.len()).map_err(|_| failed(&"sealed file evidence length exceeds u64"))
    };
    let descriptor = PartitionedFileEvidenceDescriptorV1 {
        file_key,
        file_occurrence_id: file_occurrence_id.clone(),
        segment_digest: ManifestDigest::from_sha256_bytes(&Sha256::digest(&bytes))
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?,
        segment_size_bytes: length(&bytes)?,
        decoded_size_bytes: length(&canonical)?,
        explicit_lineage: evidence.has_explicit_lineage(),
    };
    Ok((descriptor, bytes))
}

pub(super) fn decode_file_evidence_segment(
    descriptor: &PartitionedFileEvidenceDescriptorV1,
    bytes: &[u8],
) -> Result<PersistedFileEvidenceV1, CodeIndexProductionErrorV1> {
    verify_segment_identity(
        bytes,
        &descriptor.segment_digest,
        descriptor.segment_size_bytes,
        "sealed file evidence length exceeds u64",
        "sealed file evidence byte size does not match its manifest",
        "sealed file evidence digest does not match its manifest",
    )?;
    let mut canonical = Vec::new();
    inflate_file_segment(bytes, descriptor.decoded_size_bytes, &mut canonical)?;
    let evidence: PersistedFileEvidenceV1 =
        serde_json::from_slice(&canonical).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed file evidence decoding failed: {error}"
            ))
        })?;
    if evidence.has_explicit_lineage() != descriptor.explicit_lineage || evidence.is_empty() {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed file evidence does not match its descriptor".to_owned(),
        ));
    }
    Ok(evidence)
}

/// Every file's sealed evidence, restored onto `files` (in segment order):
/// cross-file edges, call limitations in sorted order, and lineage in
/// current-occurrence order. Each file's rows are expanded against that file
/// and the snapshot alone.
fn decode_file_evidence(
    generation: &PartitionedPublishedGenerationV1,
    files: &[Arc<FileGenerationArtifactsV1>],
    lineage_prior_generation: Option<&CodeGenerationId>,
    read_segment: &mut impl FnMut(
        SealedGenerationSegmentReadV1<'_>,
        &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<FileEvidenceV1, CodeIndexProductionErrorV1> {
    let present_files = generation
        .snapshot
        .files
        .iter()
        .filter(|file| file.disposition == SnapshotFileDispositionV1::Present)
        .map(|file| (file.logical_path.as_str(), &file.file_occurrence_id))
        .collect::<HashMap<_, _>>();
    let files_by_key = generation
        .file_segments
        .iter()
        .zip(files)
        .map(|(descriptor, file)| (descriptor.file_key, file))
        .collect::<HashMap<_, _>>();
    let with_evidence = generation
        .file_evidence
        .iter()
        .map(|descriptor| descriptor.file_key)
        .collect::<BTreeSet<_>>();
    let without_evidence = generation
        .file_segments
        .iter()
        .zip(files)
        .filter(|(descriptor, _)| !with_evidence.contains(&descriptor.file_key))
        .map(|(_, file)| file)
        .collect::<Vec<_>>();
    // One reused buffer per window slot, as file pages decode.
    let mut buffers =
        vec![Vec::new(); CodeIndexPublishedGenerationV1::partitioned_decode_window_files()];
    let mut restored = FileEvidenceV1::default();
    for lineage in collect_bounded_ordered(&without_evidence, |file| {
        identity_lineage(
            file,
            lineage_prior_generation,
            &generation.manifest.generation_id,
        )
    })? {
        restored.lineage.extend(lineage);
    }
    for descriptors in generation.file_evidence.chunks(buffers.len()) {
        let mut owners = Vec::with_capacity(descriptors.len());
        for (descriptor, buffer) in descriptors.iter().zip(buffers.iter_mut()) {
            let file = files_by_key.get(&descriptor.file_key).ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed file evidence names a file without a segment".to_owned(),
                )
            })?;
            buffer.clear();
            read_segment(
                SealedGenerationSegmentReadV1::Whole {
                    digest: &descriptor.segment_digest,
                    size_bytes: descriptor.segment_size_bytes,
                },
                buffer,
            )?;
            owners.push((descriptor, *file));
        }
        let segments = owners
            .into_iter()
            .zip(&buffers)
            .map(|((descriptor, file), bytes)| (descriptor, file, bytes.as_slice()))
            .collect::<Vec<_>>();
        let expanded = collect_bounded_ordered(&segments, |(descriptor, file, bytes)| {
            decode_file_evidence_segment(descriptor, bytes)?.expand(
                file,
                &present_files,
                lineage_prior_generation,
                &generation.manifest.generation_id,
            )
        })?;
        for evidence in expanded {
            restored.cross_file_edges.extend(evidence.cross_file_edges);
            restored.unresolved_calls.extend(evidence.unresolved_calls);
            restored.lineage.extend(evidence.lineage);
        }
    }
    restored.unresolved_calls.sort();
    restored
        .lineage
        .sort_by(|left, right| left.current_occurrence.cmp(&right.current_occurrence));
    Ok(restored)
}

/// Decode a segment whose bytes already verified against the manifest.
fn decode_verified_file_segment(
    descriptor: &PartitionedFileSegmentDescriptorV1,
    generation_id: &CodeGenerationId,
    snapshot_digest: &ManifestDigest,
    scope: &FileScopeIdentityV1,
    bytes: &[u8],
    restored: &mut Vec<u8>,
) -> Result<PersistedFileGenerationArtifactsV1, CodeIndexProductionErrorV1> {
    let mut canonical = Vec::new();
    {
        let _span = tracing::trace_span!("code_index.restore.segment_inflate").entered();
        inflate_file_segment(bytes, descriptor.decoded_size_bytes, &mut canonical)
    }?;
    let segment: PartitionedRawFileSegmentV1 = {
        let _span = tracing::trace_span!("code_index.restore.segment_parse").entered();
        serde_json::from_slice(&canonical).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed file segment decoding failed: {error}"
            ))
        })
    }?;
    if segment.format_revision != FILE_SEGMENT_FORMAT_REVISION {
        return Err(CodeIndexProductionErrorV1::SealedRowContractRefused {
            revision: segment.format_revision,
            message: "sealed file segment revision is not the one this build writes".to_owned(),
        });
    }
    let symbol_occurrences =
        bind_symbol_occurrences(&descriptor.file_occurrence_id, &segment.symbol_identities)?;
    let mut policy = FileSegmentDecodePolicyV1 {
        generation_id: generation_id.as_str(),
        snapshot_digest: snapshot_digest.as_str(),
        file_occurrence_id: descriptor.file_occurrence_id.as_str(),
        symbol_occurrences: &symbol_occurrences,
    };
    restored.clear();
    let identity_restore = {
        let _span = tracing::trace_span!("code_index.restore.segment_identity_restore").entered();
        canonicalize_json_into(segment.file.get().as_bytes(), &mut policy, restored)
    };
    identity_restore?;
    let payload_decoding_failed = |error: serde_json::Error| {
        // The payload already parsed as canonical JSON under its verified
        // digest, so a data-shaped refusal (missing or unknown field) is an
        // older writer's row contract, not damaged bytes.
        if error.classify() == serde_json::error::Category::Data {
            return CodeIndexProductionErrorV1::SealedRowContractRefused {
                revision: segment.format_revision,
                message: format!("sealed file segment payload decoding failed: {error}"),
            };
        }
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed file segment payload decoding failed: {error}"
        ))
    };
    let mut file: PersistedFileGenerationArtifactsV1 = {
        let _span =
            tracing::trace_span!("code_index.restore.segment_typed_deserialize_expand").entered();
        serde_json::from_slice::<PersistedFileGenerationArtifactsV2>(restored)
            .map_err(payload_decoding_failed)
            .and_then(|file| file.expand(scope, &symbol_occurrences, &segment.symbol_identities))
    }?;
    {
        let _span = tracing::trace_span!("code_index.restore.segment_artifact_sorts").entered();
        {
            file.artifacts
                .symbols
                .sort_by(|left, right| left.occurrence.cmp(&right.occurrence));
            file.artifacts.edges.sort_by(|left, right| {
                crate::chunks::canonical_edge_key(left)
                    .cmp(&crate::chunks::canonical_edge_key(right))
            });
            file.artifacts.clone_bodies.sort_by(|left, right| {
                left.occurrence
                    .symbol_occurrence_id
                    .cmp(&right.occurrence.symbol_occurrence_id)
            });
            file.artifacts.unresolved_references.sort();
        }
    };
    Ok(file)
}

pub(super) fn decode_generation_evidence(
    descriptor: &PartitionedGenerationEvidenceDescriptorV1,
    mut read_segment: impl FnMut(
        SealedGenerationSegmentReadV1<'_>,
        &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<PartitionedGenerationEvidenceV1, CodeIndexProductionErrorV1> {
    let mut reader = PartitionedEvidencePageReaderV1::new(descriptor, &mut read_segment);
    let decoding_failure = |error: serde_json::Error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation evidence payload decoding failed: {error}"
        ))
    };
    let (decoded, unread) = {
        let mut evidence = (&mut reader).take(descriptor.evidence_size_bytes);
        let decoded = {
            let _span = tracing::trace_span!("code_index.restore.evidence_stream").entered();
            serde_json::from_reader::<_, PartitionedGenerationEvidenceV1>(&mut evidence)
                .map_err(decoding_failure)
        };
        (decoded, evidence.limit())
    };
    if let Some(error) = reader.take_read_error() {
        return Err(error);
    }
    if unread != 0 {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation evidence ended before its manifest boundary".to_owned(),
        ));
    }
    decoded
}

/// Validate the descriptor layout shared by the authenticated full-manifest
/// parser and the retention-only streaming projection. Authentication stays
/// with their respective callers; this helper only establishes the bounded,
/// canonical layout they both rely on.
fn validate_partitioned_generation_layout<'a, I, J, K>(
    file_segments: I,
    snapshot_files: J,
    evidence_segment_size_bytes: u64,
    pages: K,
) -> Result<(), CodeIndexProductionErrorV1>
where
    I: ExactSizeIterator<Item = (u32, &'a FileOccurrenceId)>,
    J: Iterator<Item = (usize, &'a FileOccurrenceId)>,
    K: ExactSizeIterator<Item = (u32, u64)>,
{
    let snapshot_files = snapshot_files.collect::<Vec<_>>();
    if file_segments.len() != snapshot_files.len() {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation segment count does not match its snapshot".to_owned(),
        ));
    }
    for ((file_key, segment_file), (snapshot_key, snapshot_file)) in
        file_segments.zip(snapshot_files)
    {
        let snapshot_key = u32::try_from(snapshot_key).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation file key exceeds u32".to_owned(),
            )
        })?;
        if file_key != snapshot_key || segment_file != snapshot_file {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation file segments are not canonically keyed".to_owned(),
            ));
        }
    }
    if pages.len() == 0 {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation evidence has no pages".to_owned(),
        ));
    }
    let page_max = u64::try_from(GENERATION_EVIDENCE_PAGE_MAX_BYTES_V1).map_err(|_| {
        CodeIndexProductionErrorV1::Contract(
            "sealed generation evidence page bound exceeds u64".to_owned(),
        )
    })?;
    let mut evidence_size_bytes = 0_u64;
    for (expected_ordinal, (page_ordinal, page_size_bytes)) in pages.enumerate() {
        let expected_ordinal = u32::try_from(expected_ordinal).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation evidence page count exceeds u32".to_owned(),
            )
        })?;
        if page_ordinal != expected_ordinal || page_size_bytes == 0 || page_size_bytes > page_max {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation evidence pages are not canonically bounded and ordered"
                    .to_owned(),
            ));
        }
        evidence_size_bytes = evidence_size_bytes
            .checked_add(page_size_bytes)
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed generation evidence segment length exceeds u64".to_owned(),
                )
            })?;
    }
    if evidence_size_bytes != evidence_segment_size_bytes {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation evidence segment byte size does not match its pages".to_owned(),
        ));
    }
    Ok(())
}

/// File evidence segments are keyed by present snapshot files, at most one
/// per file, in file key order.
fn validate_file_evidence_layout<'a>(
    evidence: impl Iterator<Item = (u32, &'a FileOccurrenceId)>,
    present_files: impl Iterator<Item = (usize, &'a FileOccurrenceId)>,
) -> Result<(), CodeIndexProductionErrorV1> {
    let present = present_files.collect::<HashMap<_, _>>();
    let mut previous = None;
    for (file_key, occurrence) in evidence {
        let canonical = previous.is_none_or(|previous| previous < file_key)
            && usize::try_from(file_key)
                .ok()
                .and_then(|key| present.get(&key))
                .is_some_and(|present| *present == occurrence);
        if !canonical {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed file evidence is not canonically keyed".to_owned(),
            ));
        }
        previous = Some(file_key);
    }
    Ok(())
}

fn validate_code_graph_page_layout<'a, I, J>(
    pages: I,
    snapshot_files: J,
) -> Result<(), CodeIndexProductionErrorV1>
where
    I: ExactSizeIterator<Item = (u32, &'a FileOccurrenceId, &'a str, u64)>,
    J: ExactSizeIterator<Item = (usize, &'a FileOccurrenceId, &'a str)>,
{
    if pages.len() != snapshot_files.len() {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed code graph page count does not match its snapshot".to_owned(),
        ));
    }
    for (
        (file_key, page_occurrence, page_path, size_bytes),
        (snapshot_key, snapshot_occurrence, snapshot_path),
    ) in pages.zip(snapshot_files)
    {
        let snapshot_key = u32::try_from(snapshot_key).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed code graph page key exceeds u32".to_owned(),
            )
        })?;
        if file_key != snapshot_key
            || page_occurrence != snapshot_occurrence
            || page_path != snapshot_path
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed code graph pages are not canonically keyed".to_owned(),
            ));
        }
        if size_bytes == 0 {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed code graph page segment is empty".to_owned(),
            ));
        }
    }
    Ok(())
}

impl PartitionedPublishedGenerationV1 {
    fn sources(&self) -> super::lexical_page_source::SealedGenerationSourcesV1 {
        super::lexical_page_source::SealedGenerationSourcesV1 {
            repository_parse_identity: self.repository_parse_identity.clone(),
            ignored_source_admissions: self.ignored_source_admissions.clone(),
            ignored_source_admissions_digest: self.ignored_source_admissions_digest.clone(),
        }
    }
}

pub(super) fn parse_partitioned_manifest(
    bytes: &[u8],
) -> Result<PartitionedPublishedGenerationV1, CodeIndexProductionErrorV1> {
    let raw: PartitionedRawEnvelopeV1 = {
        let _span = tracing::trace_span!("code_index.restore.manifest_envelope_parse").entered();
        serde_json::from_slice(bytes).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation manifest decoding failed: {error}"
            ))
        })
    }?;
    let actual_digest = {
        let _span = tracing::trace_span!("code_index.restore.manifest_digest").entered();
        ManifestDigest::from_sha256_bytes(&Sha256::digest(raw.generation.get().as_bytes()))
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
    }?;
    if actual_digest != raw.state_digest {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation manifest state digest does not match its payload".to_owned(),
        ));
    }
    let probe: PartitionedFormatProbeV1 = {
        let _span = tracing::trace_span!("code_index.restore.manifest_revision_probe").entered();
        serde_json::from_str(raw.generation.get()).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation manifest format probe failed: {error}"
            ))
        })
    }?;
    match probe.format_revision {
        SEALED_GENERATION_FORMAT_REVISION_V1 => {}
        // Every other revision is a manifest this build refuses to read. A
        // retired one names a shape the writer no longer emits, so the caller
        // rebuilds the generation from its source tree instead of decoding
        // it; a revision at or above the current one was written by a newer
        // build, which this one cannot reason about.
        revision if revision < SEALED_GENERATION_FORMAT_REVISION_V1 => {
            return Err(superseded_sealed_generation_revision(revision));
        }
        _ => {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation manifest format revision is incompatible".to_owned(),
            ));
        }
    }
    let generation: PartitionedPublishedGenerationV1 = {
        let _span = tracing::trace_span!("code_index.restore.manifest_payload_parse").entered();
        serde_json::from_str(raw.generation.get()).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation manifest payload decoding failed: {error}"
            ))
        })
    }?;
    validate_partitioned_generation_layout(
        generation
            .file_segments
            .iter()
            .map(|segment| (segment.file_key, &segment.file_occurrence_id)),
        generation
            .snapshot
            .files
            .iter()
            .enumerate()
            .filter(|(_, file)| file.disposition == SnapshotFileDispositionV1::Present)
            .map(|(key, file)| (key, &file.file_occurrence_id)),
        generation.generation_evidence.segment_size_bytes,
        generation
            .generation_evidence
            .pages
            .iter()
            .map(|page| (page.page_ordinal, page.page_size_bytes)),
    )?;
    validate_file_evidence_layout(
        generation
            .file_evidence
            .iter()
            .map(|evidence| (evidence.file_key, &evidence.file_occurrence_id)),
        generation
            .snapshot
            .files
            .iter()
            .enumerate()
            .filter(|(_, file)| file.disposition == SnapshotFileDispositionV1::Present)
            .map(|(key, file)| (key, &file.file_occurrence_id)),
    )?;
    validate_code_graph_page_layout(
        generation.code_graph_pages.iter().map(|page| {
            (
                page.file_key,
                &page.file_occurrence_id,
                page.logical_path.as_str(),
                page.size_bytes,
            )
        }),
        generation
            .snapshot
            .files
            .iter()
            .enumerate()
            .map(|(key, file)| (key, &file.file_occurrence_id, file.logical_path.as_str())),
    )?;
    generation.resolution_index.validate()?;
    Ok(generation)
}

pub(super) fn snapshot_file_keys<'a>(
    file_occurrences: impl Iterator<Item = &'a FileOccurrenceId>,
) -> Result<HashMap<&'a FileOccurrenceId, u32>, CodeIndexProductionErrorV1> {
    let mut keys = HashMap::new();
    for (key, occurrence) in file_occurrences.enumerate() {
        let key = u32::try_from(key).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation file key exceeds u32".to_owned(),
            )
        })?;
        if keys.insert(occurrence, key).is_some() {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation snapshot repeats a file occurrence".to_owned(),
            ));
        }
    }
    Ok(keys)
}

type LexicalSegmentReaderV1 = dyn FnMut(
        &ManifestDigest,
        u64,
        &mut Vec<u8>,
        &dyn CodeIndexExecutionControlV1,
    ) -> Result<(), CodeIndexProductionErrorV1>
    + Send;

pub(super) struct PartitionedLexicalFileSourceV1 {
    generation_id: CodeGenerationId,
    snapshot_digest: ManifestDigest,
    scope: FileScopeIdentityV1,
    descriptors: Vec<PartitionedFileSegmentDescriptorV1>,
    read_segment: Box<LexicalSegmentReaderV1>,
}

impl std::fmt::Debug for PartitionedLexicalFileSourceV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PartitionedLexicalFileSourceV1")
            .field("generation_id", &self.generation_id)
            .field("file_count", &self.descriptors.len())
            .finish_non_exhaustive()
    }
}

impl PartitionedLexicalFileSourceV1 {
    pub(super) fn len(&self) -> usize {
        self.descriptors.len()
    }

    /// Every input the content of the pages this source emits depends on
    /// apart from the generation's route identity: each file segment in
    /// order (its key, occurrence, content address, and symbol identities).
    /// Worktrees that seal identical trees agree on it; where pages are cut
    /// can still differ with route identity widths, which changes no row.
    pub(super) fn content_digest(&self, hasher: &mut Sha256) {
        hasher.update((self.descriptors.len() as u64).to_le_bytes());
        for descriptor in &self.descriptors {
            hasher.update(descriptor.file_key.to_le_bytes());
            for field in [
                descriptor.file_occurrence_id.as_str(),
                descriptor.segment_digest.as_str(),
                descriptor.symbol_identities_digest.as_str(),
            ] {
                hasher.update((field.len() as u64).to_le_bytes());
                hasher.update(field.as_bytes());
            }
        }
    }

    /// Each file's snapshot key, in file ordinal order.
    pub(super) fn file_keys(&self) -> impl Iterator<Item = u32> + '_ {
        self.descriptors
            .iter()
            .map(|descriptor| descriptor.file_key)
    }

    /// Whether file `ordinal` seals the same segment as `parent`'s file
    /// `parent_ordinal`, so both emit the same pages.
    pub(super) fn same_segment(
        &self,
        ordinal: usize,
        parent: &Self,
        parent_ordinal: usize,
    ) -> bool {
        match (
            self.descriptors.get(ordinal),
            parent.descriptors.get(parent_ordinal),
        ) {
            (Some(child), Some(parent)) => {
                child.segment_digest == parent.segment_digest
                    && child.file_occurrence_id == parent.file_occurrence_id
                    && child.symbol_identities_digest == parent.symbol_identities_digest
            }
            _ => false,
        }
    }

    pub(super) fn lexical_byte_offsets(&self) -> Result<Vec<u64>, CodeIndexProductionErrorV1> {
        let mut offsets = Vec::with_capacity(self.descriptors.len().saturating_add(1));
        offsets.push(0_u64);
        for descriptor in &self.descriptors {
            let next = offsets
                .last()
                .copied()
                .and_then(|offset| offset.checked_add(descriptor.segment_size_bytes))
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "partitioned lexical byte total exceeds u64".to_owned(),
                    )
                })?;
            offsets.push(next);
        }
        Ok(offsets)
    }

    pub(super) fn maximum_file_bytes(&self) -> u64 {
        self.descriptors
            .iter()
            .map(|descriptor| descriptor.decoded_size_bytes)
            .max()
            .unwrap_or(0)
    }

    pub(super) fn retained_layout_bytes(&self) -> usize {
        self.descriptors.iter().fold(
            self.descriptors
                .capacity()
                .saturating_mul(std::mem::size_of::<PartitionedFileSegmentDescriptorV1>())
                .saturating_add(self.generation_id.as_str().len())
                .saturating_add(self.scope.retained_bytes()),
            |bytes, descriptor| {
                bytes
                    .saturating_add(descriptor.segment_digest.as_str().len())
                    .saturating_add(descriptor.file_occurrence_id.as_str().len())
                    .saturating_add(descriptor.symbol_identities_digest.as_str().len())
            },
        )
    }

    pub(super) fn read_window(
        &mut self,
        start: usize,
        maximum_files: usize,
        maximum_bytes: u64,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<Vec<Arc<FileGenerationArtifactsV1>>, CodeIndexProductionErrorV1> {
        let Self {
            generation_id,
            snapshot_digest,
            scope,
            descriptors,
            read_segment,
        } = self;
        let descriptors = descriptors.get(start..).ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "sealed lexical file ordinal is unavailable".to_owned(),
            )
        })?;
        let mut buffers = vec![Vec::new(); maximum_files.max(1)];
        let read = read_segment_window(
            descriptors,
            &mut buffers,
            maximum_bytes,
            |descriptor, segment| {
                checkpoint(control)?;
                (read_segment)(
                    &descriptor.segment_digest,
                    descriptor.segment_size_bytes,
                    segment,
                    control,
                )?;
                checkpoint(control)
            },
        )?;
        if read == 0 {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed lexical file window is empty".to_owned(),
            ));
        }
        restore_file_pages(decode_segment_window(
            &descriptors[..read],
            &buffers[..read],
            generation_id,
            snapshot_digest,
            scope,
        )?)
    }
}

/// Read the next window of segment bytes on the calling thread into
/// `buffers`, one slot per file: at least one file, then as many as fit within
/// `buffers.len()` and `maximum_bytes` of decoded segment JSON, the bytes the
/// window's decode materializes. Returns how many leading descriptors
/// were read. Callers keep the same slots across windows, so a decode never
/// holds more than `buffers.len()` segments and each slot grows only to the
/// largest segment it has read (the bound
/// `partitioned_codec_has_stable_bytes_and_round_trips` asserts). The reader
/// callback owns cancellation checkpoints and the actual store read.
fn read_segment_window(
    descriptors: &[PartitionedFileSegmentDescriptorV1],
    buffers: &mut [Vec<u8>],
    maximum_bytes: u64,
    mut read_segment: impl FnMut(
        &PartitionedFileSegmentDescriptorV1,
        &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<usize, CodeIndexProductionErrorV1> {
    let mut read = 0;
    let mut bytes = 0u64;
    for (descriptor, buffer) in descriptors.iter().zip(buffers) {
        if read > 0 && bytes.saturating_add(descriptor.decoded_size_bytes) > maximum_bytes {
            break;
        }
        buffer.clear();
        {
            let _span = tracing::trace_span!("code_index.restore.segment_read").entered();
            read_segment(descriptor, buffer)
        }?;
        bytes = bytes.saturating_add(descriptor.decoded_size_bytes);
        read += 1;
    }
    Ok(read)
}

/// Verify and decode one window of segment bytes on the indexing pool.
///
/// Reading is sequential and cheap (the store hands back bytes); verifying a
/// segment against its manifest digest and re-materializing its JSON is the
/// CPU-bound part, and doing it on the calling thread serialized ~4 ms per
/// file ahead of the parallel page restore, 3.4 s of a 770-file build.
/// Files are independent, so the whole window is one ordered fan-out with the
/// lowest-index failure reported, exactly as the sequential loop did.
fn decode_segment_window(
    descriptors: &[PartitionedFileSegmentDescriptorV1],
    segments: &[Vec<u8>],
    generation_id: &CodeGenerationId,
    snapshot_digest: &ManifestDigest,
    scope: &FileScopeIdentityV1,
) -> Result<Vec<PersistedFileGenerationArtifactsV1>, CodeIndexProductionErrorV1> {
    let window = descriptors.iter().zip(segments).collect::<Vec<_>>();
    collect_bounded_ordered(&window, |(descriptor, segment)| {
        let mut restored = Vec::new();
        decode_file_segment(
            descriptor,
            generation_id,
            snapshot_digest,
            scope,
            segment,
            &mut restored,
        )
    })
}

/// The key of the pages a generation's segment roster decodes to: every
/// input a page restore reads besides the generation and snapshot markers,
/// which are provenance. Worktrees and generations sealing identical content
/// share it.
fn decoded_content_digest(
    scope: &FileScopeIdentityV1,
    segments: &[PartitionedFileSegmentDescriptorV1],
) -> Result<ManifestDigest, CodeIndexProductionErrorV1> {
    let mut hasher = Sha256::new();
    hasher.update(b"tracedecay.decoded-generation-content.v1\0");
    hasher.update(FILE_SEGMENT_FORMAT_REVISION.to_le_bytes());
    scope.update_content_digest(&mut hasher);
    for segment in segments {
        hasher.update(segment.file_occurrence_id.as_str().as_bytes());
        hasher.update(b"\0");
        hasher.update(segment.segment_digest.as_str().as_bytes());
        hasher.update(b"\0");
        hasher.update(segment.decoded_size_bytes.to_le_bytes());
    }
    ManifestDigest::from_sha256_bytes(&hasher.finalize())
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
}

/// Read and decode every file segment of `generation`, one bounded window at
/// a time, into restored file pages.
fn decode_file_pages(
    generation: &PartitionedPublishedGenerationV1,
    scope: &FileScopeIdentityV1,
    read_segment: &mut impl FnMut(
        SealedGenerationSegmentReadV1<'_>,
        &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<Vec<Arc<FileGenerationArtifactsV1>>, CodeIndexProductionErrorV1> {
    let mut files = Vec::with_capacity(generation.file_segments.len());
    // One segment buffer per window slot, reused across windows: the decode
    // holds at most `partitioned_decode_window_files()` segments, each buffer
    // grown only to the largest segment its slot has read.
    let mut buffers =
        vec![Vec::new(); CodeIndexPublishedGenerationV1::partitioned_decode_window_files()];
    while files.len() < generation.file_segments.len() {
        let pending = &generation.file_segments[files.len()..];
        let read = read_segment_window(
            pending,
            &mut buffers,
            LEXICAL_FILE_PREFETCH_BYTES_V1,
            |descriptor, segment| {
                read_segment(
                    SealedGenerationSegmentReadV1::Whole {
                        digest: &descriptor.segment_digest,
                        size_bytes: descriptor.segment_size_bytes,
                    },
                    segment,
                )
            },
        )?;
        files.extend(decode_segment_window(
            &pending[..read],
            &buffers[..read],
            &generation.manifest.generation_id,
            &generation.manifest.snapshot_digest,
            scope,
        )?);
    }
    {
        let _span = tracing::trace_span!("code_index.sealed_decode.page_restore").entered();
        restore_file_pages(files)
    }
}

/// Reads one sealed segment's bytes into the buffer it is handed.
pub type SealedGenerationSegmentReaderV1<'a> = dyn FnMut(SealedGenerationSegmentReadV1<'_>, &mut Vec<u8>) -> Result<(), CodeIndexProductionErrorV1>
    + 'a;

/// Files one window of a sealed generation's segments decodes at a time.
pub(super) const FILE_WINDOW_FILES_PER_WORKER_V1: usize = 4;

/// A sealed generation's file segments, decoded back one bounded window of
/// files at a time without assembling the generation.
///
/// Only the authenticated partitioned manifest is resident: the snapshot and
/// the ordered segment descriptors. Every window's segments are verified
/// against their content addresses as they decode, and a window's decoded
/// rows are the caller's to drop before the next window is read.
pub struct SealedGenerationFileWindowsV1 {
    generation: PartitionedPublishedGenerationV1,
}

impl std::fmt::Debug for SealedGenerationFileWindowsV1 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SealedGenerationFileWindowsV1")
            .field("generation_id", &self.generation.manifest.generation_id)
            .field("files", &self.generation.file_segments.len())
            .finish_non_exhaustive()
    }
}

impl SealedGenerationFileWindowsV1 {
    /// Authenticates the partitioned manifest `manifest_bytes` and indexes its
    /// file segments. No segment is read.
    pub fn open(manifest_bytes: &[u8]) -> Result<Self, CodeIndexProductionErrorV1> {
        let generation = parse_partitioned_manifest(manifest_bytes)?;
        Ok(Self { generation })
    }

    #[must_use]
    pub fn generation_id(&self) -> &CodeGenerationId {
        &self.generation.manifest.generation_id
    }

    #[must_use]
    pub fn snapshot(&self) -> &SanitizedCodeSnapshotV1 {
        &self.generation.snapshot
    }

    #[must_use]
    pub fn manifest(&self) -> &CodeGenerationManifestV1 {
        &self.generation.manifest
    }

    pub(crate) fn code_graph_pages(&self) -> &[PartitionedCodeGraphPageDescriptorV1] {
        &self.generation.code_graph_pages
    }

    pub(crate) fn read_code_graph_page(
        &self,
        descriptor: &PartitionedCodeGraphPageDescriptorV1,
        read_segment: &mut SealedGenerationSegmentReaderV1<'_>,
    ) -> Result<super::graph_pages::PersistedCodeGraphPageV1, CodeIndexProductionErrorV1> {
        let expected = self
            .generation
            .code_graph_pages
            .get(descriptor.file_key as usize)
            .filter(|expected| *expected == descriptor)
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed code graph page descriptor is outside its generation".to_owned(),
                )
            })?;
        let mut encoded = Vec::new();
        read_segment(
            SealedGenerationSegmentReadV1::Whole {
                digest: &expected.page_digest,
                size_bytes: expected.size_bytes,
            },
            &mut encoded,
        )?;
        verify_segment_identity(
            &encoded,
            &expected.page_digest,
            expected.size_bytes,
            "sealed code graph page length exceeds u64",
            "sealed code graph page byte size does not match its manifest",
            "sealed code graph page digest does not match its manifest",
        )?;
        let page: super::graph_pages::PersistedCodeGraphPageV1 = serde_json::from_slice(&encoded)
            .map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed code graph page decoding failed: {error}"
            ))
        })?;
        if page.file.file_occurrence_id != expected.file_occurrence_id
            || page.file.logical_path != expected.logical_path
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed code graph page identity does not match its descriptor".to_owned(),
            ));
        }
        Ok(page)
    }
}

/// Serialize one partitioned manifest inside its state-digest envelope.
pub(super) fn seal_partitioned_manifest(
    generation: &PartitionedPublishedGenerationRefV1<'_>,
) -> Result<Vec<u8>, CodeIndexProductionErrorV1> {
    let generation_bytes = {
        let _span = tracing::trace_span!("code_index.sealed_encode.manifest_serialize").entered();
        serde_json::to_vec(generation).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation manifest serialization failed: {error}"
            ))
        })
    }?;
    let state_digest = {
        let _span = tracing::trace_span!("code_index.sealed_encode.manifest_digest").entered();
        ManifestDigest::from_sha256_bytes(&Sha256::digest(&generation_bytes))
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
    }?;
    {
        let _span = tracing::trace_span!("code_index.sealed_encode.manifest_envelope").entered();
        {
            let generation =
                RawValue::from_string(String::from_utf8(generation_bytes).map_err(|error| {
                    CodeIndexProductionErrorV1::Contract(format!(
                        "sealed generation manifest is not UTF-8: {error}"
                    ))
                })?)
                .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
            serde_json::to_vec(&PartitionedEnvelopeRefV1 {
                state_digest: &state_digest,
                generation: &generation,
            })
            .map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation manifest serialization failed: {error}"
                ))
            })
        }
    }
}

impl VerifiedSealedLexicalPageSourceV1 {
    pub fn open_partitioned_sealed(
        manifest_bytes: &[u8],
        source_state_digest: ManifestDigest,
        read_segment: impl FnMut(
            &ManifestDigest,
            u64,
            &mut Vec<u8>,
            &dyn CodeIndexExecutionControlV1,
        ) -> Result<(), CodeIndexProductionErrorV1>
        + Send
        + 'static,
        maximum_page_chunks: usize,
        maximum_page_bytes: usize,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let generation = parse_partitioned_manifest(manifest_bytes)?;
        let chunk_policy = generation.chunk_policy.clone();
        let sources = generation.sources();
        let source = PartitionedLexicalFileSourceV1 {
            generation_id: generation.manifest.generation_id.clone(),
            snapshot_digest: generation.manifest.snapshot_digest.clone(),
            scope: FileScopeIdentityV1::of(&generation.manifest, &generation.snapshot),
            descriptors: generation.file_segments,
            read_segment: Box::new(read_segment),
        };
        Self::open_partitioned_parts(
            generation.manifest,
            generation.snapshot,
            generation.statistics,
            chunk_policy,
            sources,
            source,
            source_state_digest,
            maximum_page_chunks,
            maximum_page_bytes,
        )
    }
}

impl CodeIndexPublishedGenerationV1 {
    pub fn encode_partitioned_sealed(
        &self,
        publish_segment: impl FnMut(
            SealedGenerationSegmentPublicationV1<'_>,
        ) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<Vec<u8>, CodeIndexProductionErrorV1> {
        self.encode_partitioned_sealed_with_parent(None, publish_segment)
    }

    pub fn encode_partitioned_sealed_with_parent(
        &self,
        parent_manifest_bytes: Option<&[u8]>,
        mut publish_segment: impl FnMut(
            SealedGenerationSegmentPublicationV1<'_>,
        ) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<Vec<u8>, CodeIndexProductionErrorV1> {
        self.validate()?;
        // Segment reuse is an optimization over a readable parent, never a
        // precondition for publishing. A parent sealed in a shape this build
        // has retired therefore offers no reuse and the child re-encodes its
        // own segments, refusing the publication instead would leave a store
        // that holds a retired generation unable to replace it.
        let parent = match parent_manifest_bytes
            .map(parse_partitioned_manifest)
            .transpose()
        {
            Ok(parent) => parent,
            Err(CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(_)) => None,
            Err(error) => return Err(error),
        };
        if let Some(parent) = parent.as_ref()
            && self.manifest.parent_generation.as_ref() != Some(&parent.manifest.generation_id)
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed segment reuse parent does not match the generation manifest".to_owned(),
            ));
        }
        let parent_segments = parent
            .as_ref()
            .map(|parent| {
                parent
                    .file_segments
                    .iter()
                    .filter_map(|descriptor| {
                        parent
                            .snapshot
                            .files
                            .get(descriptor.file_key as usize)
                            .map(|file| (&file.file_occurrence_id, (file, descriptor)))
                    })
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();
        let reusable_graph_page_digests = parent
            .as_ref()
            .map(|parent| {
                parent
                    .code_graph_pages
                    .iter()
                    .map(|page| page.page_digest.clone())
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let reusable_index_digests = parent
            .as_ref()
            .map(|parent| {
                parent
                    .resolution_index
                    .segments()
                    .map(|page| page.segment_digest.clone())
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let file_keys = snapshot_file_keys(
            self.snapshot
                .files
                .iter()
                .map(|file| &file.file_occurrence_id),
        )?;
        let mut file_segments = Vec::with_capacity(self.files.len());
        let buffers = SealedEncodeBufferPoolV1::default();
        let scope = FileScopeIdentityV1::of(&self.manifest, &self.snapshot);
        let plan_file = |file: &FileGenerationArtifactsV1| -> Result<
            FileSegmentPlanV1,
            CodeIndexProductionErrorV1,
        > {
            let key = file_keys
                .get(&file.extraction.file_occurrence_id)
                .copied()
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed generation file is absent from its snapshot".to_owned(),
                    )
                })?;
            let current_snapshot_file = self.snapshot.files.get(key as usize).ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed generation file key is outside its snapshot".to_owned(),
                )
            })?;
            let prior = parent_segments.get(&current_snapshot_file.file_occurrence_id);
            let current_identities_digest = prior
                .map(|_| {
                    let mut identities = file
                        .artifacts
                        .symbols
                        .iter()
                        .map(|symbol| symbol.identity.clone())
                        .collect::<Vec<_>>();
                    identities.sort();
                    identities.dedup();
                    symbol_identities_digest(&identities)
                })
                .transpose()?;
            let reused = prior
                .and_then(|(prior_file, prior_descriptor)| {
                    (*prior_file == current_snapshot_file).then_some(())?;
                    let language = current_snapshot_file.language.as_ref()?;
                    let current_extractor_revision = self
                        .manifest
                        .extractor_revisions
                        .iter()
                        .find(|(candidate, _)| candidate == language)
                        .map(|(_, revision)| revision)?;
                    let prior_extractor_revision = parent
                        .as_ref()?
                        .manifest
                        .extractor_revisions
                        .iter()
                        .find(|(candidate, _)| candidate == language)
                        .map(|(_, revision)| revision)?;
                    (prior_extractor_revision == current_extractor_revision).then_some(())?;
                    (current_identities_digest.as_ref()
                        == Some(&prior_descriptor.symbol_identities_digest))
                    .then_some(())?;
                    let mut descriptor = (*prior_descriptor).clone();
                    descriptor.file_key = key;
                    Some(descriptor)
                });
            if let Some(descriptor) = reused {
                return Ok(FileSegmentPlanV1::Reused(descriptor));
            }
            let mut encoder = PartitionedSegmentEncoderV1 {
                payload: buffers.take(),
                segment: buffers.take(),
            };
            let descriptor =
                encoder.encode_file_segment(&self.manifest.generation_id, &scope, file, key)?;
            buffers.give(std::mem::take(&mut encoder.payload));
            Ok(FileSegmentPlanV1::Encoded(descriptor, encoder.segment))
        };
        // Files are independent, so each window is one ordered fan-out on the
        // indexing pool (lowest-index failure, panic containment, CPU
        // admission per unit); the window then publishes serially in file
        // order, so segment bytes, on-disk order, and every digest are the
        // ones the sequential loop produced. Windowing bounds the encoded
        // bytes held in memory to a few segments per worker; the serial loop
        // held one.
        // ponytail: the window bound is a file count, not bytes; add a byte
        // bound like `read_segment_window` if a few huge files ever matter.
        let window_files = crate::parallelism::indexing_workers()
            .max(1)
            .saturating_mul(SEALED_ENCODE_WINDOW_FILES_PER_WORKER_V1);
        for window in self.files.chunks(window_files) {
            let plans = {
                let _span = tracing::trace_span!("code_index.sealed_encode.file_window").entered();
                collect_bounded_ordered(window, |file| plan_file(file))
            }?;
            for plan in plans {
                let descriptor = match plan {
                    FileSegmentPlanV1::Reused(descriptor) => descriptor,
                    FileSegmentPlanV1::Encoded(descriptor, bytes) => {
                        publish_segment(SealedGenerationSegmentPublicationV1::File {
                            digest: &descriptor.segment_digest,
                            bytes: &bytes,
                        })?;
                        buffers.give(bytes);
                        descriptor
                    }
                };
                file_segments.push(descriptor);
            }
        }
        file_segments.sort_by_key(|segment| segment.file_key);
        let reusable_file_evidence_digests = parent
            .as_ref()
            .map(|parent| {
                parent
                    .file_evidence
                    .iter()
                    .map(|evidence| evidence.segment_digest.clone())
                    .collect::<BTreeSet<_>>()
            })
            .unwrap_or_default();
        let (lineage_prior_generation, file_evidence) = {
            let _span = tracing::trace_span!("code_index.sealed_encode.file_evidence").entered();
            self.encode_file_evidence(
                &file_keys,
                reusable_file_evidence_digests,
                &mut publish_segment,
            )
        }?;
        let rosters = FileChunkRostersV1::new(
            self.files
                .iter()
                .map(|file| {
                    file_keys
                        .get(&file.extraction.file_occurrence_id)
                        .map(|key| (*key, file.artifacts.chunks.chunks.as_slice()))
                        .ok_or_else(|| {
                            CodeIndexProductionErrorV1::Contract(
                                "sealed generation file is absent from its snapshot".to_owned(),
                            )
                        })
                })
                .collect::<Result<Vec<_>, _>>()?
                .into_iter(),
        );
        let generation_evidence = write_generation_evidence(
            &PartitionedGenerationEvidenceRefV1::new(
                &self.projection,
                &rosters,
                lineage_prior_generation.as_ref(),
            )?,
            &mut publish_segment,
        )?;
        drop(rosters);
        let mut code_graph_pages = Vec::with_capacity(self.snapshot.files.len());
        {
            let _span = tracing::trace_span!("code_index.sealed_encode.graph_pages").entered();
            {
                self.for_each_sealed_code_graph_page(|page| {
                    code_graph_pages.push(publish_code_graph_page(
                        page,
                        &reusable_graph_page_digests,
                        &mut publish_segment,
                    )?);
                    Ok(())
                })
            }
        }?;
        let resolution_index = seal_resolution_index(
            &self.files,
            &mut |publication: SealedGenerationSegmentPublicationV1<'_>| match publication {
                SealedGenerationSegmentPublicationV1::ResolutionIndex { digest, .. }
                    if reusable_index_digests.contains(digest) =>
                {
                    Ok(())
                }
                publication => publish_segment(publication),
            },
        )?;
        let statistics = self.generation_statistics()?;
        seal_partitioned_manifest(&PartitionedPublishedGenerationRefV1 {
            format_revision: SEALED_GENERATION_FORMAT_REVISION_V1,
            manifest: &self.manifest,
            snapshot: &self.snapshot,
            statistics: &statistics,
            chunk_count: u64::try_from(self.chunks.chunks().len()).map_err(|_| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed generation chunk count exceeds u64".to_owned(),
                )
            })?,
            chunk_policy: self.chunk_policy_summary(),
            repository_parse_identity: &self.repository_parse_identity,
            ignored_source_admissions: self.ignored_source_roster.admissions(),
            ignored_source_admissions_digest: self.ignored_source_roster.digest(),
            file_segments: &file_segments,
            file_evidence: &file_evidence,
            coverage: self.coverage,
            capability: &self.capability,
            generation_evidence: &generation_evidence,
            code_graph_pages: &code_graph_pages,
            resolution_index: &resolution_index,
        })
    }

    /// Seal each file's cross-file evidence as its own segment, publishing
    /// only bytes neither the parent nor an earlier file of this seal stored.
    /// Returns the prior generation implicit lineage rows continue from and
    /// the descriptors in file key order.
    fn encode_file_evidence(
        &self,
        file_keys: &HashMap<&FileOccurrenceId, u32>,
        mut stored: BTreeSet<ManifestDigest>,
        mut publish_segment: impl FnMut(
            SealedGenerationSegmentPublicationV1<'_>,
        ) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<
        (
            Option<CodeGenerationId>,
            Vec<PartitionedFileEvidenceDescriptorV1>,
        ),
        CodeIndexProductionErrorV1,
    > {
        let (lineage_prior_generation, persisted) = compact_file_evidence(
            &self.files,
            &self.edges,
            &self.unresolved_calls,
            &self.lineage,
            &self.manifest.generation_id,
        )?;
        let owned = self
            .files
            .iter()
            .zip(&persisted)
            .filter(|(_, evidence)| !evidence.is_empty())
            .collect::<Vec<_>>();
        let encoded = collect_bounded_ordered(&owned, |(file, evidence)| {
            let occurrence = &file.extraction.file_occurrence_id;
            let file_key = file_keys.get(occurrence).copied().ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed file evidence file is absent from its snapshot".to_owned(),
                )
            })?;
            encode_file_evidence_segment(file_key, occurrence, evidence)
        })?;
        let mut descriptors = Vec::with_capacity(encoded.len());
        for (descriptor, bytes) in encoded {
            if stored.insert(descriptor.segment_digest.clone()) {
                publish_segment(SealedGenerationSegmentPublicationV1::FileEvidence {
                    digest: &descriptor.segment_digest,
                    bytes: &bytes,
                })?;
            }
            descriptors.push(descriptor);
        }
        descriptors.sort_by_key(|descriptor| descriptor.file_key);
        Ok((lineage_prior_generation, descriptors))
    }

    /// File segments [`Self::decode_partitioned_sealed`] holds in memory at
    /// once: one reusable segment buffer per indexing worker. Its file
    /// allocation is therefore bounded by this many buffers, each no larger
    /// than the largest file segment it read.
    ///
    /// Left at bare `workers` rather than the restore-window multiplier used
    /// by `fill_admitted_window`/`read_window`
    /// (`LEXICAL_DECODE_WINDOW_FILES_PER_WORKER_V1`): this function only
    /// bounds `decode_partitioned_sealed`'s monolithic in-memory rehydration
    /// path, which no measured span drives through the drain-window
    /// fan-out that motivated the multiplier (`index-bench` reaches the
    /// lazy `VerifiedSealedLexicalPageSourceV1` restore paths, never this
    /// one). Widening it here would only grow buffer-pool memory without
    /// cutting any observed `install()` call count, and it would silently
    /// disable the cross-window buffer-reuse coverage this function's
    /// callers test against fixed small fixtures.
    #[must_use]
    pub fn partitioned_decode_window_files() -> usize {
        crate::parallelism::indexing_workers().max(1)
    }

    /// Decode a sealed generation. Its file pages come from `content` when
    /// another generation already decoded the same segment roster, and are
    /// admitted there otherwise, so identical content is decoded and held
    /// once.
    pub fn decode_partitioned_sealed(
        bytes: &[u8],
        content: &SharedDecodedContentPoolV1,
        mut read_segment: impl FnMut(
            SealedGenerationSegmentReadV1<'_>,
            &mut Vec<u8>,
        ) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let mut probe = DecodePeakProbeV1::start();
        let generation = {
            let _span = tracing::trace_span!("code_index.restore.manifest").entered();
            parse_partitioned_manifest(bytes)
        }?;
        let scope = FileScopeIdentityV1::of(&generation.manifest, &generation.snapshot);
        let digest = decoded_content_digest(&scope, &generation.file_segments)?;
        let content = match content.lookup(&digest) {
            Some(shared) => shared,
            None => content.admit(DecodedGenerationContentV1::new(
                digest,
                decode_file_pages(&generation, &scope, &mut read_segment)?,
            )),
        };
        probe.sample();
        let evidence = {
            let _span = tracing::trace_span!("code_index.restore.generation_evidence").entered();
            decode_generation_evidence(&generation.generation_evidence, &mut read_segment)
        }?;
        probe.sample();
        let files = &content.files;
        let FileEvidenceV1 {
            cross_file_edges,
            unresolved_calls,
            lineage,
        } = {
            let _span = tracing::trace_span!("code_index.restore.file_evidence").entered();
            decode_file_evidence(
                &generation,
                files,
                evidence.lineage_prior_generation.as_ref(),
                &mut read_segment,
            )
        }?;
        probe.sample();
        let (projection_request, projection_receipt) = {
            let _span = tracing::trace_span!("code_index.restore.evidence_expand").entered();
            {
                let rosters =
                    FileChunkRostersV1::new(generation.file_segments.iter().zip(files).map(
                        |(descriptor, file)| {
                            (descriptor.file_key, file.artifacts.chunks.chunks.as_slice())
                        },
                    ));
                let request = evidence.projection_request.expand(&rosters)?;
                let receipt = evidence.projection_receipt.expand(&request)?;
                Ok::<_, CodeIndexProductionErrorV1>((request, receipt))
            }
        }?;
        probe.sample();
        assemble_published_generation(
            StreamingPersistedPublishedGenerationV1 {
                manifest: generation.manifest,
                snapshot: generation.snapshot,
                repository_parse_identity: generation.repository_parse_identity,
                ignored_source_admissions: generation.ignored_source_admissions,
                ignored_source_admissions_digest: generation.ignored_source_admissions_digest,
                content,
                lineage,
                coverage: generation.coverage,
                capability: generation.capability,
                projection_request,
                projection_receipt,
                cross_file_edges,
                unresolved_calls,
            },
            probe,
        )
    }

    /// Authenticate only the tiny partitioned manifest and return the metadata
    /// needed to bind already-published text and graph owners. Segment bytes
    /// remain untouched; callers may use this only when those owners already
    /// have their own verified durable artifacts.
    pub fn partitioned_text_metadata(
        bytes: &[u8],
    ) -> Result<VerifiedSealedTextGenerationMetadataV1, CodeIndexProductionErrorV1> {
        Self::partitioned_metadata_and_lane(bytes).map(|(metadata, _)| metadata)
    }

    /// [`Self::partitioned_text_metadata`] with the generation's lane digest.
    pub(super) fn partitioned_metadata_and_lane(
        bytes: &[u8],
    ) -> Result<(VerifiedSealedTextGenerationMetadataV1, ManifestDigest), CodeIndexProductionErrorV1>
    {
        let generation = parse_partitioned_manifest(bytes)?;
        let lane = super::sparse_increment::lane_digest(
            &generation.snapshot,
            &generation.file_segments,
            &generation.code_graph_pages,
        )?;
        let sources = generation.sources();
        let metadata = VerifiedSealedTextGenerationMetadataV1::from_partitioned_manifest(
            generation.manifest,
            generation.snapshot,
            generation.statistics,
            generation.chunk_policy,
            sources,
        )?;
        Ok((metadata, lane))
    }

    pub fn partitioned_segment_identities(
        bytes: &[u8],
    ) -> Result<Vec<SealedGenerationSegmentIdentityV1>, CodeIndexProductionErrorV1> {
        let generation = parse_partitioned_manifest(bytes)?;
        let mut identities = generation
            .file_segments
            .into_iter()
            .map(|segment| SealedGenerationSegmentIdentityV1 {
                digest: segment.segment_digest,
                size_bytes: segment.segment_size_bytes,
            })
            .collect::<Vec<_>>();
        identities.extend(generation.file_evidence.into_iter().map(|evidence| {
            SealedGenerationSegmentIdentityV1 {
                digest: evidence.segment_digest,
                size_bytes: evidence.segment_size_bytes,
            }
        }));
        identities.push(SealedGenerationSegmentIdentityV1 {
            digest: generation.generation_evidence.segment_digest,
            size_bytes: generation.generation_evidence.segment_size_bytes,
        });
        identities.extend(generation.code_graph_pages.into_iter().map(|page| {
            SealedGenerationSegmentIdentityV1 {
                digest: page.page_digest,
                size_bytes: page.size_bytes,
            }
        }));
        identities.extend(generation.resolution_index.segments().map(|segment| {
            SealedGenerationSegmentIdentityV1 {
                digest: segment.segment_digest.clone(),
                size_bytes: segment.segment_size_bytes,
            }
        }));
        Ok(identities)
    }

    /// Stream only current-revision segment descriptors from a generation
    /// manifest.
    ///
    /// This projection intentionally does not re-materialize the enclosing
    /// manifest. Retention must first authenticate the complete outer file
    /// against its content-addressed name. It must never replace
    /// [`Self::verify_partitioned_sealed`] at a serving boundary.
    ///
    /// Unlike the decoding readers, a revision this build does not write is
    /// abstained rather than refused: retention marks the segments it can
    /// prove live and must stay able to plan a store that still holds a
    /// retired generation, whose own segments are then unreferenced.
    ///
    /// That abstention is deliberately asymmetric, and it is what keeps a
    /// store holding a retired generation collectable: the generation's
    /// segments become sweepable while its manifest is still retained, so a
    /// retained retired manifest can outlive the segments it addresses. It is
    /// fail-safe only because every decoding reader refuses that manifest at
    /// the revision gate before requesting one segment, nothing can observe
    /// the missing bytes. A future revision that decoded a retired manifest
    /// instead of refusing it would have to mark its segments live here first.
    pub fn partitioned_segment_identities_from_reader(
        reader: impl Read,
    ) -> Result<Option<Vec<SealedGenerationSegmentIdentityV1>>, CodeIndexProductionErrorV1> {
        let envelope: PartitionedSegmentIdentityEnvelopeV1 = serde_json::from_reader(reader)
            .map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation segment descriptor decoding failed: {error}"
                ))
            })?;
        let generation = envelope.generation;
        if generation.format_revision != SEALED_GENERATION_FORMAT_REVISION_V1 {
            return Ok(None);
        }
        validate_partitioned_generation_layout(
            generation
                .file_segments
                .iter()
                .map(|segment| (segment.file_key, &segment.file_occurrence_id)),
            generation
                .snapshot
                .files
                .iter()
                .enumerate()
                .filter(|(_, file)| file.disposition == SnapshotFileDispositionV1::Present)
                .map(|(key, file)| (key, &file.file_occurrence_id)),
            generation.generation_evidence.segment_size_bytes,
            generation
                .generation_evidence
                .pages
                .iter()
                .map(|page| (page.page_ordinal, page.page_size_bytes)),
        )?;
        validate_file_evidence_layout(
            generation
                .file_evidence
                .iter()
                .map(|evidence| (evidence.file_key, &evidence.file_occurrence_id)),
            generation
                .snapshot
                .files
                .iter()
                .enumerate()
                .filter(|(_, file)| file.disposition == SnapshotFileDispositionV1::Present)
                .map(|(key, file)| (key, &file.file_occurrence_id)),
        )?;
        validate_code_graph_page_layout(
            generation.code_graph_pages.iter().map(|page| {
                (
                    page.file_key,
                    &page.file_occurrence_id,
                    page.logical_path.as_str(),
                    page.size_bytes,
                )
            }),
            generation
                .snapshot
                .files
                .iter()
                .enumerate()
                .map(|(key, file)| (key, &file.file_occurrence_id, file.logical_path.as_str())),
        )?;
        let mut identities = Vec::with_capacity(
            generation
                .file_segments
                .len()
                .saturating_add(generation.file_evidence.len())
                .saturating_add(generation.code_graph_pages.len())
                .saturating_add(1),
        );
        for (digest, size_bytes) in generation
            .file_segments
            .into_iter()
            .map(|segment| (segment.segment_digest, segment.segment_size_bytes))
            .chain(
                generation
                    .file_evidence
                    .into_iter()
                    .map(|evidence| (evidence.segment_digest, evidence.segment_size_bytes)),
            )
        {
            identities.push(SealedGenerationSegmentIdentityV1 { digest, size_bytes });
        }
        identities.push(SealedGenerationSegmentIdentityV1 {
            digest: generation.generation_evidence.segment_digest,
            size_bytes: generation.generation_evidence.segment_size_bytes,
        });
        identities.extend(generation.code_graph_pages.into_iter().map(|page| {
            SealedGenerationSegmentIdentityV1 {
                digest: page.page_digest,
                size_bytes: page.size_bytes,
            }
        }));
        let resolution_index = generation.resolution_index.ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation has no resolution index".to_owned(),
            )
        })?;
        resolution_index.validate()?;
        identities.extend(resolution_index.segments().map(|segment| {
            SealedGenerationSegmentIdentityV1 {
                digest: segment.segment_digest.clone(),
                size_bytes: segment.segment_size_bytes,
            }
        }));
        Ok(Some(identities))
    }

    pub fn verify_partitioned_sealed(
        bytes: &[u8],
        mut read_segment: impl FnMut(
            SealedGenerationSegmentReadV1<'_>,
            &mut Vec<u8>,
        ) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let generation = parse_partitioned_manifest(bytes)?;
        let mut segment = Vec::new();
        let per_file = generation
            .file_segments
            .iter()
            .map(|descriptor| (&descriptor.segment_digest, descriptor.segment_size_bytes))
            .chain(
                generation
                    .file_evidence
                    .iter()
                    .map(|descriptor| (&descriptor.segment_digest, descriptor.segment_size_bytes)),
            )
            .chain(
                generation
                    .resolution_index
                    .segments()
                    .map(|segment| (&segment.segment_digest, segment.segment_size_bytes)),
            );
        for (digest, size_bytes) in per_file {
            segment.clear();
            read_segment(
                SealedGenerationSegmentReadV1::Whole { digest, size_bytes },
                &mut segment,
            )?;
            verify_segment_identity(
                &segment,
                digest,
                size_bytes,
                "sealed generation segment length exceeds u64",
                "sealed generation segment does not match its content address",
                "sealed generation segment does not match its content address",
            )?;
        }
        let mut evidence = PartitionedEvidencePageReaderV1::new(
            &generation.generation_evidence,
            &mut read_segment,
        );
        std::io::copy(&mut evidence, &mut std::io::sink()).map_err(|error| {
            evidence.take_read_error().unwrap_or_else(|| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation evidence verification failed: {error}"
                ))
            })
        })?;
        evidence.finish()?;
        for descriptor in &generation.code_graph_pages {
            segment.clear();
            read_segment(
                SealedGenerationSegmentReadV1::Whole {
                    digest: &descriptor.page_digest,
                    size_bytes: descriptor.size_bytes,
                },
                &mut segment,
            )?;
            verify_segment_identity(
                &segment,
                &descriptor.page_digest,
                descriptor.size_bytes,
                "sealed code graph page length exceeds u64",
                "sealed code graph page byte size does not match its manifest",
                "sealed code graph page digest does not match its manifest",
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    #[test]
    fn streamed_segment_projection_refuses_an_incomplete_descriptor_set() {
        let digest = format!("sha256:{}", "0".repeat(64));
        let manifest = serde_json::json!({
            "state_digest": digest,
            "generation": {
                "format_revision": SEALED_GENERATION_FORMAT_REVISION_V1,
                "snapshot": {
                    "files": [
                        {
                            "file_occurrence_id": FIXTURE_FILE,
                            "logical_path": "src/lib.rs",
                            "disposition": "present"
                        },
                        {
                            "file_occurrence_id": "file.partitioned.missing",
                            "logical_path": "src/missing.rs",
                            "disposition": "present"
                        }
                    ]
                },
                "file_segments": [{
                    "file_key": 0,
                    "segment_digest": format!("sha256:{}", "1".repeat(64)),
                    "segment_size_bytes": 12,
                    "file_occurrence_id": FIXTURE_FILE
                }],
                "generation_evidence": {
                    "segment_digest": format!("sha256:{}", "2".repeat(64)),
                    "segment_size_bytes": 8,
                    "pages": [{
                        "page_ordinal": 0,
                        "page_digest": format!("sha256:{}", "3".repeat(64)),
                        "page_size_bytes": 8
                    }]
                }
            }
        });
        let bytes = serde_json::to_vec(&manifest).expect("encode malformed manifest");

        let error = CodeIndexPublishedGenerationV1::partitioned_segment_identities_from_reader(
            bytes.as_slice(),
        )
        .expect_err("a partial descriptor set must not authorize segment sweeping");
        assert!(
            error.to_string().contains("segment count"),
            "unexpected projection error: {error}"
        );
    }

    #[test]
    fn missing_or_null_evidence_pages_are_rejected_by_both_readers() {
        let missing = serde_json::json!({
            "segment_digest": "sha256:56f954431e92b5e2ef9b1355bc229acf516a8d3409b7e48e9cd9fb7856411f29",
            "segment_size_bytes": 1
        });
        let explicit_null = serde_json::json!({
            "segment_digest": "sha256:56f954431e92b5e2ef9b1355bc229acf516a8d3409b7e48e9cd9fb7856411f29",
            "segment_size_bytes": 1,
            "pages": null
        });
        for descriptor in [missing.clone(), explicit_null.clone()] {
            assert!(
                serde_json::from_value::<PartitionedGenerationEvidenceDescriptorV1>(
                    descriptor.clone()
                )
                .is_err(),
                "the full parser requires a page table: {descriptor}"
            );
        }
        assert!(
            serde_json::from_value::<PartitionedEvidenceSegmentIdentityV1>(explicit_null).is_err(),
            "the retention reader refuses an explicit null page table"
        );
        let identity: PartitionedEvidenceSegmentIdentityV1 =
            serde_json::from_value(missing).expect("retention reaches its revision gate");
        assert!(identity.pages.is_empty());
        let file = FileOccurrenceId::new("file.partitioned.only").unwrap();
        let error = validate_partitioned_generation_layout(
            [(0, &file)].into_iter(),
            [&file].into_iter().enumerate(),
            identity.segment_size_bytes,
            identity
                .pages
                .iter()
                .map(|page| (page.page_ordinal, page.page_size_bytes)),
        )
        .expect_err("a current descriptor without pages is malformed");
        assert!(error.to_string().contains("has no pages"), "{error}");
    }

    #[test]
    fn shared_descriptor_layout_validator_keeps_parser_and_retention_invariants_aligned() {
        let first = FileOccurrenceId::new("file.partitioned.first").unwrap();
        let second = FileOccurrenceId::new("file.partitioned.second").unwrap();
        let files = [&first, &second];
        let valid_pages = [(0, 4_u64), (1, 5_u64)];
        validate_partitioned_generation_layout(
            [(0, &first), (1, &second)].into_iter(),
            files.into_iter().enumerate(),
            9,
            valid_pages.into_iter(),
        )
        .expect("current paged descriptor is canonical");

        for (segments, snapshot, size, pages, expected) in [
            (
                vec![(0, &first)],
                vec![&first, &second],
                4,
                vec![(0, 4)],
                "segment count",
            ),
            (
                vec![(0, &second), (1, &first)],
                vec![&first, &second],
                4,
                vec![(0, 4)],
                "canonically keyed",
            ),
            (
                vec![(0, &first), (1, &second)],
                vec![&first, &second],
                4,
                vec![],
                "has no pages",
            ),
            (
                vec![(0, &first), (1, &second)],
                vec![&first, &second],
                4,
                vec![(1, 4)],
                "canonically bounded and ordered",
            ),
            (
                vec![(0, &first), (1, &second)],
                vec![&first, &second],
                4,
                vec![(0, 0)],
                "canonically bounded and ordered",
            ),
            (
                vec![(0, &first), (1, &second)],
                vec![&first, &second],
                4,
                vec![(
                    0,
                    u64::try_from(GENERATION_EVIDENCE_PAGE_MAX_BYTES_V1).unwrap() + 1,
                )],
                "canonically bounded and ordered",
            ),
            (
                vec![(0, &first), (1, &second)],
                vec![&first, &second],
                5,
                vec![(0, 4)],
                "byte size does not match",
            ),
        ] {
            let error = validate_partitioned_generation_layout(
                segments.into_iter(),
                snapshot.into_iter().enumerate(),
                size,
                pages.into_iter(),
            )
            .expect_err("malformed descriptor must be rejected by both readers");
            assert!(
                error.to_string().contains(expected),
                "expected {expected:?}, got {error}"
            );
        }
    }

    /// The replaced `serde_json::Value` encoder. It survives only here, as the
    /// byte-identity authority the streaming writer is measured against;
    /// production carries exactly one encoder.
    mod reference {
        use super::*;

        #[derive(Serialize)]
        pub(super) struct ReferenceFileSegmentV1 {
            pub(super) format_revision: u32,
            pub(super) symbol_identities: Vec<String>,
            pub(super) file: Value,
        }

        fn normalize_identity_fields(
            value: &mut Value,
            field: IdentityFieldV1,
            generation_id: &str,
            file_occurrence_id: &str,
            symbol_keys: &HashMap<&str, u32>,
        ) {
            match value {
                Value::String(identity) => match field {
                    IdentityFieldV1::Generation if identity == generation_id => {
                        *identity = GENERATION_ID_MARKER.to_owned();
                    }
                    IdentityFieldV1::FileOccurrence if identity == file_occurrence_id => {
                        *identity = FILE_OCCURRENCE_ID_MARKER.to_owned();
                    }
                    IdentityFieldV1::SymbolOccurrence => {
                        if let Some(key) = symbol_keys.get(identity.as_str()) {
                            *identity = format!("{SYMBOL_OCCURRENCE_ID_MARKER_PREFIX}{key}");
                        }
                    }
                    IdentityFieldV1::Other
                    | IdentityFieldV1::Generation
                    | IdentityFieldV1::SnapshotDigest
                    | IdentityFieldV1::FileOccurrence => {}
                },
                Value::Array(values) => {
                    for value in values {
                        normalize_identity_fields(
                            value,
                            field,
                            generation_id,
                            file_occurrence_id,
                            symbol_keys,
                        );
                    }
                }
                Value::Object(values) => {
                    for (key, value) in values {
                        normalize_identity_fields(
                            value,
                            identity_field(key),
                            generation_id,
                            file_occurrence_id,
                            symbol_keys,
                        );
                    }
                }
                _ => {}
            }
        }

        /// The whole replaced file-segment encode, verbatim apart from taking
        /// its stable symbols as plain pairs.
        pub(super) fn file_segment_bytes(
            payload: &impl Serialize,
            generation_id: &str,
            file_occurrence_id: &str,
            stable_symbols: &[(String, String)],
        ) -> (Vec<u8>, Vec<String>) {
            let mut value = serde_json::to_value(payload).expect("reference payload value");
            let ordered_symbols = stable_symbols
                .iter()
                .map(|(identity, occurrence)| (identity.as_str(), occurrence.as_str()))
                .collect::<BTreeMap<_, _>>();
            let symbol_occurrences = ordered_symbols
                .values()
                .map(|occurrence| (*occurrence).to_owned())
                .collect::<Vec<_>>();
            let symbol_keys = symbol_occurrences
                .iter()
                .enumerate()
                .map(|(key, occurrence)| {
                    (
                        occurrence.as_str(),
                        u32::try_from(key).expect("reference symbol key"),
                    )
                })
                .collect::<HashMap<_, _>>();
            normalize_identity_fields(
                &mut value,
                IdentityFieldV1::Other,
                generation_id,
                file_occurrence_id,
                &symbol_keys,
            );
            for field in ["edges", "unresolved_references"] {
                if let Some(rows) = value
                    .get_mut("artifacts")
                    .and_then(|artifacts| artifacts.get_mut(field))
                    .and_then(Value::as_array_mut)
                {
                    rows.sort_by_cached_key(Value::to_string);
                }
            }
            drop(symbol_keys);
            let bytes = serde_json::to_vec(&ReferenceFileSegmentV1 {
                format_revision: FILE_SEGMENT_FORMAT_REVISION,
                symbol_identities: ordered_symbols
                    .keys()
                    .map(|identity| (*identity).to_owned())
                    .collect(),
                file: value,
            })
            .expect("reference segment bytes");
            let file_occurrence_id =
                FileOccurrenceId::new(file_occurrence_id).expect("reference file identity");
            let symbol_occurrences = ordered_symbols
                .keys()
                .map(|identity| {
                    crate::chunks::symbol_occurrence_id(
                        &file_occurrence_id,
                        &SymbolIdentityDigest::new(*identity).expect("reference symbol identity"),
                    )
                    .expect("reference symbol occurrence")
                    .as_str()
                    .to_owned()
                })
                .collect();
            (bytes, symbol_occurrences)
        }
    }

    const FIXTURE_GENERATION: &str = "generation.partitioned.fixture";
    const FIXTURE_FILE: &str = "file.partitioned.fixture";

    /// Field order here is deliberately unsorted at every level so the
    /// canonical key-order rule is load bearing, and the strings carry escapes
    /// so the borrowed fast path and the unescaping path are both exercised.
    #[derive(Serialize)]
    struct FixturePayload {
        authority: FixtureAuthority,
        extraction: FixtureExtraction,
        artifacts: FixtureArtifacts,
    }

    #[derive(Serialize)]
    struct FixtureAuthority {
        logical_path: String,
        #[serde(rename = "project\"id")]
        project_id: String,
        worktree_id: Option<String>,
    }

    #[derive(Serialize)]
    struct FixtureExtraction {
        generation_id: String,
        file_occurrence_id: String,
        language: String,
        parsed_ranges: Vec<[u32; 2]>,
    }

    #[derive(Serialize)]
    struct FixtureArtifacts {
        chunks: FixtureChunks,
        symbols: Vec<FixtureSymbol>,
        edges: Vec<FixtureEdge>,
        unresolved_references: Vec<FixtureUnresolved>,
        imports: Vec<String>,
    }

    #[derive(Serialize)]
    struct FixtureChunks {
        chunks: Vec<FixtureChunk>,
        rows_digest: String,
    }

    #[derive(Serialize)]
    struct FixtureChunk {
        chunk_id: String,
        symbol_occurrence_ids: Vec<String>,
        text: String,
        generation_id: String,
    }

    #[derive(Serialize)]
    struct FixtureSymbol {
        occurrence: String,
        identity: String,
        file_occurrence_id: String,
    }

    #[derive(Serialize)]
    struct FixtureEdge {
        to_occurrence: String,
        from_occurrence: String,
        kind: String,
    }

    #[derive(Serialize)]
    struct FixtureUnresolved {
        name: String,
        alternatives: Vec<String>,
    }

    fn occurrence(index: usize) -> String {
        format!("symbol.partitioned.fixture.{index:02}")
    }

    fn fixture_payload() -> FixturePayload {
        FixturePayload {
            authority: FixtureAuthority {
                logical_path: "src/\u{2603}/\"quoted\"\tpath.rs".to_owned(),
                project_id: "project.partitioned.fixture".to_owned(),
                worktree_id: None,
            },
            extraction: FixtureExtraction {
                generation_id: FIXTURE_GENERATION.to_owned(),
                file_occurrence_id: FIXTURE_FILE.to_owned(),
                language: "rust".to_owned(),
                parsed_ranges: vec![[0, 12], [12, 40]],
            },
            artifacts: FixtureArtifacts {
                chunks: FixtureChunks {
                    chunks: vec![
                        FixtureChunk {
                            chunk_id: "chunk.partitioned.fixture.00".to_owned(),
                            symbol_occurrence_ids: vec![occurrence(3), occurrence(1)],
                            text: "fn alpha() {}\n".to_owned(),
                            generation_id: FIXTURE_GENERATION.to_owned(),
                        },
                        FixtureChunk {
                            chunk_id: "chunk.partitioned.fixture.01".to_owned(),
                            // A symbol-shaped string that is not a known
                            // occurrence must survive verbatim.
                            symbol_occurrence_ids: vec![occurrence(9)],
                            text: "fn beta() {}\n".to_owned(),
                            generation_id: "generation.partitioned.other".to_owned(),
                        },
                    ],
                    rows_digest: "sha256:fixture".to_owned(),
                },
                symbols: vec![
                    FixtureSymbol {
                        occurrence: occurrence(3),
                        identity: "symbol::zulu".to_owned(),
                        file_occurrence_id: FIXTURE_FILE.to_owned(),
                    },
                    FixtureSymbol {
                        occurrence: occurrence(1),
                        identity: "symbol::alpha".to_owned(),
                        file_occurrence_id: FIXTURE_FILE.to_owned(),
                    },
                    FixtureSymbol {
                        occurrence: occurrence(2),
                        identity: "symbol::alpha".to_owned(),
                        file_occurrence_id: "file.partitioned.other".to_owned(),
                    },
                ],
                edges: vec![
                    FixtureEdge {
                        to_occurrence: occurrence(1),
                        from_occurrence: occurrence(3),
                        kind: "calls".to_owned(),
                    },
                    FixtureEdge {
                        to_occurrence: occurrence(3),
                        from_occurrence: occurrence(1),
                        kind: "calls".to_owned(),
                    },
                    FixtureEdge {
                        to_occurrence: occurrence(2),
                        from_occurrence: occurrence(1),
                        kind: "contains".to_owned(),
                    },
                ],
                unresolved_references: vec![
                    FixtureUnresolved {
                        name: "zulu".to_owned(),
                        alternatives: vec![occurrence(2), occurrence(1)],
                    },
                    FixtureUnresolved {
                        name: "alpha".to_owned(),
                        alternatives: Vec::new(),
                    },
                ],
                imports: vec!["std::fmt".to_owned()],
            },
        }
    }

    fn fixture_stable_symbols() -> Vec<(String, String)> {
        fixture_payload()
            .artifacts
            .symbols
            .iter()
            .map(|symbol| {
                let identity = format!("{}:{}", symbol.identity, symbol.occurrence);
                (
                    ManifestDigest::from_sha256_bytes(&Sha256::digest(identity.as_bytes()))
                        .expect("fixture symbol digest")
                        .as_str()
                        .to_owned(),
                    symbol.occurrence.clone(),
                )
            })
            .collect()
    }

    fn streamed_file_segment() -> (Vec<u8>, PartitionedFileSegmentDescriptorV1) {
        let payload = fixture_payload();
        let stable = fixture_stable_symbols();
        let identities = stable
            .iter()
            .map(|(identity, _)| {
                SymbolIdentityDigest::new(identity.clone()).expect("fixture symbol identity")
            })
            .collect::<Vec<_>>();
        let occurrence_identities = stable
            .iter()
            .zip(&identities)
            .map(|((_, occurrence), identity)| (occurrence.as_str(), identity))
            .collect::<HashMap<_, _>>();
        let mut encoder = PartitionedSegmentEncoderV1::default();
        serde_json::to_writer(&mut encoder.payload, &payload).expect("streamed payload");
        let descriptor = encoder
            .encode_serialized_file_segment(
                FILE_SEGMENT_FORMAT_REVISION,
                &CodeGenerationId::new(FIXTURE_GENERATION).expect("fixture generation identity"),
                None,
                FileOccurrenceId::new(FIXTURE_FILE).expect("fixture file identity"),
                7,
                &occurrence_identities,
            )
            .expect("streamed file segment");
        (encoder.segment_bytes().to_vec(), descriptor)
    }

    #[test]
    fn streaming_file_segment_bytes_match_the_value_encoder() {
        let (reference_bytes, reference_occurrences) = reference::file_segment_bytes(
            &fixture_payload(),
            FIXTURE_GENERATION,
            FIXTURE_FILE,
            &fixture_stable_symbols(),
        );

        let (stored_bytes, descriptor) = streamed_file_segment();
        let mut streamed_bytes = Vec::new();
        inflate_file_segment(
            &stored_bytes,
            descriptor.decoded_size_bytes,
            &mut streamed_bytes,
        )
        .expect("stored segment inflates to its recorded size");

        assert_eq!(
            String::from_utf8(streamed_bytes.clone()).expect("streamed segment is UTF-8"),
            String::from_utf8(reference_bytes.clone()).expect("reference segment is UTF-8"),
            "the streaming writer must reproduce the canonical segment bytes"
        );
        assert!(
            stored_bytes.len() < streamed_bytes.len(),
            "the stored segment is compressed"
        );
        assert!(
            inflate_file_segment(
                &stored_bytes,
                descriptor.decoded_size_bytes - 1,
                &mut Vec::new()
            )
            .is_err(),
            "inflation past the recorded size is refused"
        );
        let segment = serde_json::from_slice::<PartitionedRawFileSegmentV1<'_>>(&streamed_bytes)
            .expect("segment envelope");
        assert_eq!(
            bind_symbol_occurrences(&descriptor.file_occurrence_id, &segment.symbol_identities)
                .expect("segment symbol occurrences")
                .iter()
                .map(|occurrence| occurrence.as_str().to_owned())
                .collect::<Vec<_>>(),
            reference_occurrences,
            "the symbol key assignment must not move"
        );
        assert_eq!(
            descriptor.symbol_identities_digest,
            symbol_identities_digest(&segment.symbol_identities).expect("identities digest"),
            "reuse compares the digest of exactly the identities the segment carries"
        );
        assert_eq!(descriptor.file_key, 7);
        assert_eq!(
            descriptor.decoded_size_bytes,
            u64::try_from(reference_bytes.len()).expect("reference length"),
        );
        assert_eq!(
            descriptor.segment_size_bytes,
            u64::try_from(stored_bytes.len()).expect("stored length"),
        );
        assert_eq!(
            descriptor.segment_digest,
            ManifestDigest::from_sha256_bytes(&Sha256::digest(&stored_bytes))
                .expect("stored digest"),
            "the content address covers the stored bytes"
        );
        assert!(
            streamed_bytes
                .windows(GENERATION_ID_MARKER.len())
                .any(|window| window == GENERATION_ID_MARKER.as_bytes()),
            "the fixture must actually exercise identity substitution"
        );
    }

    #[test]
    fn streaming_file_segment_decode_refuses_an_unknown_symbol_key() {
        let mut policy = FileSegmentDecodePolicyV1 {
            generation_id: FIXTURE_GENERATION,
            snapshot_digest: "sha256:fixture",
            file_occurrence_id: FIXTURE_FILE,
            symbol_occurrences: &[],
        };
        let mut restored = Vec::new();

        let error = canonicalize_json_into(
            br#"{"occurrence":"$tracedecay:s:4"}"#,
            &mut policy,
            &mut restored,
        )
        .expect_err("an unresolvable symbol key must be refused");

        assert!(
            error.to_string().contains("invalid symbol identity key"),
            "unexpected error: {error}"
        );
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct FixtureEvidence {
        lineage: Vec<FixtureLineage>,
        projection_request: FixtureProjectionRequest,
        padding: String,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct FixtureLineage {
        to_occurrence: String,
        from_occurrence: String,
        prior_generation: String,
        source_generation: String,
    }

    #[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
    struct FixtureProjectionRequest {
        generation_id: String,
        chunk_ids: Vec<String>,
        parent_chunk_id: Option<String>,
    }

    #[test]
    fn evidence_pages_preserve_the_exact_stream_and_content_address() {
        let evidence = FixtureEvidence {
            lineage: vec![FixtureLineage {
                to_occurrence: occurrence(1),
                from_occurrence: occurrence(3),
                prior_generation: FIXTURE_GENERATION.to_owned(),
                source_generation: "generation.partitioned.other".to_owned(),
            }],
            projection_request: FixtureProjectionRequest {
                generation_id: FIXTURE_GENERATION.to_owned(),
                chunk_ids: vec![
                    "chunk.partitioned.fixture.00".to_owned(),
                    "chunk.partitioned.fixture.unknown".to_owned(),
                ],
                parent_chunk_id: Some("chunk.partitioned.fixture.01".to_owned()),
            },
            padding: "p".repeat(GENERATION_EVIDENCE_PAGE_MAX_BYTES_V1 + 17),
        };
        let expected = serde_json::to_vec(&evidence).expect("reference evidence stream");
        let expected_digest =
            ManifestDigest::from_sha256_bytes(&Sha256::digest(&expected)).expect("stream digest");
        let mut pack = Vec::new();
        let mut published_pages = Vec::new();
        let mut commits = 0_usize;
        let mut publish = |publication: SealedGenerationSegmentPublicationV1<'_>| -> Result<
            (),
            CodeIndexProductionErrorV1,
        > {
            match publication {
                SealedGenerationSegmentPublicationV1::GenerationEvidencePage {
                    page_ordinal,
                    page_digest,
                    bytes,
                } => {
                    assert_eq!(page_ordinal as usize, published_pages.len());
                    published_pages.push((page_digest.clone(), bytes.len()));
                    pack.extend_from_slice(bytes);
                }
                SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit {
                    segment_digest,
                    segment_size_bytes,
                    page_count,
                } => {
                    commits += 1;
                    assert_eq!(segment_digest, &expected_digest);
                    assert_eq!(segment_size_bytes, expected.len() as u64);
                    assert_eq!(page_count as usize, published_pages.len());
                }
                SealedGenerationSegmentPublicationV1::File { .. }
                | SealedGenerationSegmentPublicationV1::FileEvidence { .. }
                | SealedGenerationSegmentPublicationV1::ResolutionIndex { .. }
                | SealedGenerationSegmentPublicationV1::CodeGraphPage { .. } => {
                    panic!("evidence writer cannot publish another segment kind")
                }
            }
            Ok(())
        };
        let mut writer = PartitionedEvidencePageWriterV1::new(&mut publish);
        serde_json::to_writer(&mut writer, &evidence).expect("paged evidence encode");
        let descriptor = writer
            .finish(u64::try_from(expected.len()).expect("evidence length"))
            .expect("paged evidence finish");
        drop(writer);

        assert_eq!(pack, expected, "page boundaries must not move a byte");
        assert!(
            descriptor.pages.len() > 1,
            "the exact-stream fixture must cross a page boundary"
        );
        assert_eq!(commits, 1, "all pages belong to one pack transaction");
        assert_eq!(descriptor.segment_digest, expected_digest);
        assert_eq!(
            descriptor.segment_size_bytes,
            u64::try_from(expected.len()).expect("expected evidence length")
        );

        let mut read = |request: SealedGenerationSegmentReadV1<'_>, buffer: &mut Vec<u8>| {
            let SealedGenerationSegmentReadV1::Range {
                digest,
                size_bytes,
                offset,
                length,
            } = request
            else {
                panic!("evidence reader must request a range")
            };
            assert_eq!(digest, &descriptor.segment_digest);
            assert_eq!(size_bytes, descriptor.segment_size_bytes);
            let start = usize::try_from(offset).expect("range offset");
            let end = start + usize::try_from(length).expect("range length");
            buffer.clear();
            buffer.extend_from_slice(&pack[start..end]);
            Ok(())
        };
        let mut reader = PartitionedEvidencePageReaderV1::new(&descriptor, &mut read);
        let restored: FixtureEvidence =
            serde_json::from_reader(&mut reader).expect("paged evidence decode");
        reader
            .finish()
            .expect("aggregate evidence identity verifies");
        assert_eq!(restored, evidence);

        pack[0] ^= 1;
        let mut read = |request: SealedGenerationSegmentReadV1<'_>, buffer: &mut Vec<u8>| {
            let SealedGenerationSegmentReadV1::Range { offset, length, .. } = request else {
                panic!("evidence reader must request a range")
            };
            let start = usize::try_from(offset).expect("range offset");
            let end = start + usize::try_from(length).expect("range length");
            buffer.clear();
            buffer.extend_from_slice(&pack[start..end]);
            Ok(())
        };
        let mut reader = PartitionedEvidencePageReaderV1::new(&descriptor, &mut read);
        let _: Result<FixtureEvidence, _> = serde_json::from_reader(&mut reader);
        let error = reader
            .take_read_error()
            .expect("tampered page must fail its content address");
        assert!(
            error.to_string().contains("page digest"),
            "unexpected tamper error: {error}"
        );

        let mut read_count = 0_usize;
        let mut read = |request: SealedGenerationSegmentReadV1<'_>, buffer: &mut Vec<u8>| {
            let SealedGenerationSegmentReadV1::Range { offset, length, .. } = request else {
                panic!("evidence reader must request a range")
            };
            read_count += 1;
            let start = usize::try_from(offset).expect("range offset");
            let mut end = start + usize::try_from(length).expect("range length");
            if read_count == 2 {
                end -= 1;
            }
            buffer.clear();
            buffer.extend_from_slice(&expected[start..end]);
            Ok(())
        };
        let mut reader = PartitionedEvidencePageReaderV1::new(&descriptor, &mut read);
        let _: Result<FixtureEvidence, _> = serde_json::from_reader(&mut reader);
        let error = reader
            .take_read_error()
            .expect("a missing page byte must fail closed");
        assert!(
            error.to_string().contains("page byte size"),
            "unexpected missing-page error: {error}"
        );

        // The page table is the whole attestation of a paged segment, so a
        // page past the first must be refused on its own digest rather than
        // on an aggregate the reader no longer recomputes.
        let mut tampered = expected.clone();
        let second_page_byte =
            usize::try_from(descriptor.pages[0].page_size_bytes).expect("first page size") + 1;
        tampered[second_page_byte] ^= 1;
        let mut read = |request: SealedGenerationSegmentReadV1<'_>, buffer: &mut Vec<u8>| {
            let SealedGenerationSegmentReadV1::Range { offset, length, .. } = request else {
                panic!("evidence reader must request a range")
            };
            let start = usize::try_from(offset).expect("range offset");
            let end = start + usize::try_from(length).expect("range length");
            buffer.clear();
            buffer.extend_from_slice(&tampered[start..end]);
            Ok(())
        };
        let mut reader = PartitionedEvidencePageReaderV1::new(&descriptor, &mut read);
        let _: Result<FixtureEvidence, _> = serde_json::from_reader(&mut reader);
        let error = reader
            .take_read_error()
            .expect("a tampered later page must fail its content address");
        assert!(
            error.to_string().contains("page digest"),
            "unexpected later-page tamper error: {error}"
        );
    }

    #[test]
    fn evidence_failure_after_a_page_never_emits_a_pack_commit() {
        let evidence = FixtureEvidence {
            lineage: Vec::new(),
            projection_request: FixtureProjectionRequest {
                generation_id: FIXTURE_GENERATION.to_owned(),
                chunk_ids: Vec::new(),
                parent_chunk_id: None,
            },
            padding: "p".repeat(GENERATION_EVIDENCE_PAGE_MAX_BYTES_V1 * 3),
        };
        let mut pages = 0_usize;
        let mut commits = 0_usize;
        let mut publish = |publication: SealedGenerationSegmentPublicationV1<'_>| {
            match publication {
                SealedGenerationSegmentPublicationV1::GenerationEvidencePage { .. } => {
                    pages += 1;
                    if pages == 2 {
                        return Err(CodeIndexProductionErrorV1::Contract(
                            "injected page-two failure".to_owned(),
                        ));
                    }
                }
                SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit { .. } => {
                    commits += 1;
                }
                SealedGenerationSegmentPublicationV1::File { .. }
                | SealedGenerationSegmentPublicationV1::FileEvidence { .. }
                | SealedGenerationSegmentPublicationV1::ResolutionIndex { .. }
                | SealedGenerationSegmentPublicationV1::CodeGraphPage { .. } => {
                    panic!("evidence writer cannot publish another segment kind")
                }
            }
            Ok(())
        };
        let mut writer = PartitionedEvidencePageWriterV1::new(&mut publish);
        let encoded = serde_json::to_writer(&mut writer, &evidence);
        assert!(
            encoded.is_err(),
            "the injected page failure must stop encoding"
        );
        let error = writer
            .take_publish_error()
            .expect("the typed publication failure must be retained");
        assert!(error.to_string().contains("injected page-two failure"));
        drop(writer);
        assert_eq!(pages, 2);
        assert_eq!(commits, 0, "partial evidence must never become a pack");
    }

    #[test]
    fn generation_evidence_encoding_retains_only_one_bounded_page() {
        let large_identity = "e".repeat(GENERATION_EVIDENCE_PAGE_MAX_BYTES_V1);
        let evidence = FixtureEvidence {
            lineage: (0..16)
                .map(|index| FixtureLineage {
                    to_occurrence: format!("{large_identity}.to.{index}"),
                    from_occurrence: format!("{large_identity}.from.{index}"),
                    prior_generation: format!("generation.partitioned.prior.{index}"),
                    source_generation: FIXTURE_GENERATION.to_owned(),
                })
                .collect(),
            projection_request: FixtureProjectionRequest {
                generation_id: FIXTURE_GENERATION.to_owned(),
                chunk_ids: Vec::new(),
                parent_chunk_id: None,
            },
            padding: String::new(),
        };
        let mut published_pages = 0_usize;
        let mut published_bytes = 0_usize;
        let mut largest_page = 0_usize;
        let mut commits = 0_usize;
        let mut publish = |publication: SealedGenerationSegmentPublicationV1<'_>| -> Result<
            (),
            CodeIndexProductionErrorV1,
        > {
            match publication {
                SealedGenerationSegmentPublicationV1::GenerationEvidencePage { bytes, .. } => {
                    published_pages += 1;
                    published_bytes = published_bytes.saturating_add(bytes.len());
                    largest_page = largest_page.max(bytes.len());
                }
                SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit { .. } => {
                    commits += 1;
                }
                SealedGenerationSegmentPublicationV1::File { .. }
                | SealedGenerationSegmentPublicationV1::FileEvidence { .. }
                | SealedGenerationSegmentPublicationV1::ResolutionIndex { .. }
                | SealedGenerationSegmentPublicationV1::CodeGraphPage { .. } => {
                    panic!("evidence writer cannot publish another segment kind")
                }
            }
            Ok(())
        };
        let mut writer = PartitionedEvidencePageWriterV1::new(&mut publish);
        serde_json::to_writer(&mut writer, &evidence).expect("large paged evidence encode");
        let evidence_size_bytes = writer.position();
        let descriptor = writer
            .finish(evidence_size_bytes)
            .expect("large paged evidence finish");
        let peak_page_capacity = writer.peak_page_capacity;
        let peak_retained_owned_bytes = writer.peak_retained_owned_bytes;
        drop(writer);

        assert!(
            published_bytes > GENERATION_EVIDENCE_PAGE_MAX_BYTES_V1 * 8,
            "the fixture must be materially larger than one page"
        );
        assert_eq!(published_pages, descriptor.pages.len());
        assert_eq!(commits, 1, "all pages require one pack commit");
        assert_eq!(
            descriptor
                .pages
                .iter()
                .map(|page| usize::try_from(page.page_size_bytes).expect("page size"))
                .sum::<usize>(),
            published_bytes
        );
        assert!(largest_page <= GENERATION_EVIDENCE_PAGE_MAX_BYTES_V1);
        assert_eq!(
            peak_page_capacity, GENERATION_EVIDENCE_PAGE_MAX_BYTES_V1,
            "the live page allocation must never grow with total evidence bytes"
        );
        let descriptor_bytes = descriptor
            .pages
            .capacity()
            .saturating_mul(std::mem::size_of::<PartitionedEvidencePageDescriptorV1>())
            .saturating_add(
                descriptor
                    .pages
                    .iter()
                    .map(|page| page.page_digest.as_str().len())
                    .sum::<usize>(),
            );
        assert_eq!(
            peak_retained_owned_bytes,
            GENERATION_EVIDENCE_PAGE_MAX_BYTES_V1 + descriptor_bytes,
            "the live encoder peak must include exactly one page and its descriptors"
        );
        assert!(
            peak_retained_owned_bytes * 8 < published_bytes,
            "retained encoding memory must not scale with the full {published_bytes}-byte stream"
        );
    }

    fn historical_fixture_root() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/partitioned_pre_paging")
    }

    /// The archival carrier was sealed at revision seven, which named a
    /// manifest both with and without its census and is therefore retired.
    /// Every decoding reader refuses it with the typed rebuild error rather
    /// than admitting either shape.
    #[test]
    fn archival_carrier_revision_is_refused_for_rebuild() {
        let manifest = std::fs::read(historical_fixture_root().join("manifest.json"))
            .expect("historical manifest");
        let envelope: serde_json::Value =
            serde_json::from_slice(&manifest).expect("historical manifest json");
        let revision = u32::try_from(
            envelope["generation"]["format_revision"]
                .as_u64()
                .expect("historical format revision"),
        )
        .expect("historical format revision fits u32");
        assert!(
            revision < SEALED_GENERATION_FORMAT_REVISION_V1,
            "the archival carrier must stay behind the revision this build serves"
        );
        let Err(error) = parse_partitioned_manifest(&manifest) else {
            panic!("a retired manifest revision must be refused, never migrated")
        };

        assert!(
            matches!(
                error,
                CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(refused)
                    if refused == revision
            ),
            "unexpected error: {error}"
        );
        assert!(
            error.to_string().contains("will be rebuilt from source"),
            "a retired revision must tell the operator it rebuilds: {error}"
        );
    }
}
