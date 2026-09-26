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
use super::lexical_page_source::{LEXICAL_FILE_PREFETCH_BYTES_V1, checkpoint};
use super::lineage_rows::{PersistedLineageV1, occurrence_roster};
use super::projection_rows::{
    PersistedBatchReceiptRefV1, PersistedBatchReceiptV1, PersistedProjectionRequestRefV1,
    PersistedProjectionRequestV1, chunk_roster,
};
use super::sealed_codec::{
    FileScopeIdentityV1, PersistedFileGenerationArtifactsRefV2, PersistedFileGenerationArtifactsV1,
    PersistedFileGenerationArtifactsV2, SEALED_GENERATION_FORMAT_REVISION_V1,
    StreamingPersistedPublishedGenerationV1, assemble_published_generation, restore_file_pages,
    superseded_sealed_generation_revision,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartitionedFileSegmentDescriptorV1 {
    file_key: u32,
    segment_digest: ManifestDigest,
    segment_size_bytes: u64,
    /// Length of the canonical JSON the stored bytes inflate to. Restore
    /// windows budget on it, and inflation must end at exactly this length.
    decoded_size_bytes: u64,
    file_occurrence_id: FileOccurrenceId,
    /// Digest of the ordered symbol identities the segment carries, so a
    /// successor can decide reuse without reading the segment.
    symbol_identities_digest: ManifestDigest,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartitionedEvidencePageDescriptorV1 {
    page_ordinal: u32,
    page_digest: ManifestDigest,
    page_size_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartitionedGenerationEvidenceDescriptorV1 {
    segment_digest: ManifestDigest,
    segment_size_bytes: u64,
    pages: Vec<PartitionedEvidencePageDescriptorV1>,
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
struct PartitionedPublishedGenerationRefV1<'a> {
    format_revision: u32,
    manifest: &'a CodeGenerationManifestV1,
    snapshot: &'a SanitizedCodeSnapshotV1,
    statistics: &'a CodeIndexGenerationStatisticsV1,
    repository_parse_identity: &'a CodeIndexRepositoryParseIdentityV1,
    ignored_source_admissions: &'a [CodeIndexIgnoredSourceAdmissionV1],
    ignored_source_admissions_digest: &'a ManifestDigest,
    file_segments: &'a [PartitionedFileSegmentDescriptorV1],
    coverage: CoverageSummaryV1,
    capability: &'a CodeIndexCapabilityManifestV1,
    generation_evidence: &'a PartitionedGenerationEvidenceDescriptorV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PartitionedPublishedGenerationV1 {
    /// Gated by the revision probe before this strict parse runs.
    #[serde(rename = "format_revision")]
    _format_revision: u32,
    manifest: CodeGenerationManifestV1,
    snapshot: SanitizedCodeSnapshotV1,
    statistics: CodeIndexGenerationStatisticsV1,
    repository_parse_identity: CodeIndexRepositoryParseIdentityV1,
    ignored_source_admissions: Vec<CodeIndexIgnoredSourceAdmissionV1>,
    ignored_source_admissions_digest: ManifestDigest,
    file_segments: Vec<PartitionedFileSegmentDescriptorV1>,
    coverage: CoverageSummaryV1,
    capability: CodeIndexCapabilityManifestV1,
    generation_evidence: PartitionedGenerationEvidenceDescriptorV1,
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
    generation_evidence: PartitionedEvidenceSegmentIdentityV1,
}

#[derive(Deserialize)]
struct PartitionedSegmentIdentitySnapshotV1 {
    files: Vec<PartitionedSnapshotFileIdentityV1>,
}

#[derive(Deserialize)]
struct PartitionedSnapshotFileIdentityV1 {
    file_occurrence_id: FileOccurrenceId,
    disposition: SnapshotFileDispositionV1,
}

#[derive(Deserialize)]
struct PartitionedFileSegmentIdentityV1 {
    file_key: u32,
    segment_digest: ManifestDigest,
    segment_size_bytes: u64,
    file_occurrence_id: FileOccurrenceId,
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

/// The evidence stream. Every part is in a persisted row form whose rows
/// index the generation's own symbols and chunks, so restore expands it only
/// after the file segments supply those rosters: lineage per
/// [`super::lineage_rows`], and the projection request and receipt per
/// [`super::projection_rows`].
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PartitionedGenerationEvidenceV1 {
    #[serde(deserialize_with = "deserialize_evidence_lineage")]
    lineage: PersistedLineageV1,
    #[serde(deserialize_with = "deserialize_evidence_projection_request")]
    projection_request: PersistedProjectionRequestV1,
    #[serde(deserialize_with = "deserialize_evidence_projection_receipt")]
    projection_receipt: PersistedBatchReceiptV1,
}

/// The generation evidence stream decodes on one thread, so each of its
/// three payloads is measured separately.
fn deserialize_evidence_lineage<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<PersistedLineageV1, D::Error> {
    hotpath::measure_block!(
        "code_index.restore.evidence_lineage",
        Deserialize::deserialize(deserializer)
    )
}

fn deserialize_evidence_projection_request<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<PersistedProjectionRequestV1, D::Error> {
    hotpath::measure_block!(
        "code_index.restore.evidence_projection_request",
        Deserialize::deserialize(deserializer)
    )
}

fn deserialize_evidence_projection_receipt<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<PersistedBatchReceiptV1, D::Error> {
    hotpath::measure_block!(
        "code_index.restore.evidence_projection_receipt",
        Deserialize::deserialize(deserializer)
    )
}

#[derive(Serialize)]
struct PartitionedGenerationEvidenceRefV1<'a> {
    lineage: PersistedLineageV1,
    projection_request: PersistedProjectionRequestRefV1<'a>,
    projection_receipt: PersistedBatchReceiptRefV1<'a>,
}

impl<'a> PartitionedGenerationEvidenceRefV1<'a> {
    fn new(
        generation: &'a CodeIndexPublishedGenerationV1,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let symbols = occurrence_roster(
            generation
                .files
                .iter()
                .flat_map(|file| file.artifacts.symbols.iter()),
        );
        let chunks = chunk_roster(
            generation
                .files
                .iter()
                .flat_map(|file| file.artifacts.chunks.chunks.iter()),
        );
        let request = generation.projection.request();
        Ok(Self {
            lineage: PersistedLineageV1::compact(
                &generation.lineage,
                &generation.manifest.generation_id,
                &symbols,
            )?,
            projection_request: PersistedProjectionRequestRefV1::new(request, &chunks)?,
            projection_receipt: PersistedBatchReceiptRefV1::new(
                request,
                generation.projection.receipt(),
            )?,
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
            pages: std::mem::take(&mut self.descriptors),
        })
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

impl PartitionedSegmentEncoderV1 {
    #[cfg(test)]
    fn segment_bytes(&self) -> &[u8] {
        &self.segment
    }

    #[hotpath::measure(label = "code_index.sealed_encode.file")]
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
    #[hotpath::measure(label = "code_index.sealed_encode.file_rewrite")]
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

    #[hotpath::measure(label = "code_index.sealed_encode.evidence")]
    fn encode_generation_evidence(
        &mut self,
        generation: &CodeIndexPublishedGenerationV1,
        mut publish: impl FnMut(
            SealedGenerationSegmentPublicationV1<'_>,
        ) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<PartitionedGenerationEvidenceDescriptorV1, CodeIndexProductionErrorV1> {
        // File encoding needs two reusable buffers. Release their owned
        // capacities before evidence starts so the evidence phase retains only
        // one bounded page plus its compact content-address descriptors.
        drop(std::mem::take(&mut self.payload));
        drop(std::mem::take(&mut self.segment));
        let evidence = PartitionedGenerationEvidenceRefV1::new(generation)?;
        let mut writer = PartitionedEvidencePageWriterV1::new(&mut publish);
        let encoded = serde_json::to_writer(&mut writer, &evidence);
        if let Some(error) = writer.take_publish_error() {
            return Err(error);
        }
        encoded.map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation evidence serialization failed: {error}"
            ))
        })?;
        writer.finish()
    }
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

fn decode_file_segment(
    descriptor: &PartitionedFileSegmentDescriptorV1,
    generation_id: &CodeGenerationId,
    snapshot_digest: &ManifestDigest,
    scope: &FileScopeIdentityV1,
    bytes: &[u8],
    restored: &mut Vec<u8>,
) -> Result<PersistedFileGenerationArtifactsV1, CodeIndexProductionErrorV1> {
    hotpath::measure_block!(
        "code_index.restore.segment_verify",
        verify_segment_identity(
            bytes,
            &descriptor.segment_digest,
            descriptor.segment_size_bytes,
            "sealed file segment length exceeds u64",
            "sealed file segment byte size does not match its manifest",
            "sealed file segment digest does not match its manifest",
        )
    )?;
    hotpath::measure_block!(
        "code_index.restore.segment_decode",
        decode_verified_file_segment(
            descriptor,
            generation_id,
            snapshot_digest,
            scope,
            bytes,
            restored,
        )
    )
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

/// Decode a segment whose bytes already verified against the manifest.
fn decode_verified_file_segment(
    descriptor: &PartitionedFileSegmentDescriptorV1,
    generation_id: &CodeGenerationId,
    snapshot_digest: &ManifestDigest,
    scope: &FileScopeIdentityV1,
    bytes: &[u8],
    restored: &mut Vec<u8>,
) -> Result<PersistedFileGenerationArtifactsV1, CodeIndexProductionErrorV1> {
    hotpath::gauge!("code_index.restore.segment_bytes_total").inc(bytes.len());
    let mut canonical = Vec::new();
    hotpath::measure_block!(
        "code_index.restore.segment_inflate",
        inflate_file_segment(bytes, descriptor.decoded_size_bytes, &mut canonical)
    )?;
    let segment: PartitionedRawFileSegmentV1 = hotpath::measure_block!(
        "code_index.restore.segment_parse",
        serde_json::from_slice(&canonical).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed file segment decoding failed: {error}"
            ))
        })
    )?;
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
    let identity_restore = hotpath::measure_block!(
        "code_index.restore.segment_identity_restore",
        canonicalize_json_into(segment.file.get().as_bytes(), &mut policy, restored)
    );
    hotpath::gauge!("code_index.restore.identity_restored_bytes_total").inc(restored.len());
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
    let mut file: PersistedFileGenerationArtifactsV1 = hotpath::measure_block!(
        "code_index.restore.segment_typed_deserialize_expand",
        serde_json::from_slice::<PersistedFileGenerationArtifactsV2>(restored)
            .map_err(payload_decoding_failed)
            .and_then(|file| file.expand(scope, &symbol_occurrences, &segment.symbol_identities))
    )?;
    hotpath::measure_block!("code_index.restore.segment_artifact_sorts", {
        file.artifacts
            .symbols
            .sort_by(|left, right| left.occurrence.cmp(&right.occurrence));
        file.artifacts.edges.sort_by(|left, right| {
            crate::chunks::canonical_edge_key(left).cmp(&crate::chunks::canonical_edge_key(right))
        });
        file.artifacts.clone_bodies.sort_by(|left, right| {
            left.occurrence
                .symbol_occurrence_id
                .cmp(&right.occurrence.symbol_occurrence_id)
        });
        file.artifacts.unresolved_references.sort();
    });
    Ok(file)
}

fn decode_generation_evidence(
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
    let decoded = hotpath::measure_block!(
        "code_index.restore.evidence_stream",
        serde_json::from_reader::<_, PartitionedGenerationEvidenceV1>(&mut reader)
            .map_err(decoding_failure)
    );
    if let Some(error) = reader.take_read_error() {
        return Err(error);
    }
    let evidence = decoded?;
    reader.finish()?;
    Ok(evidence)
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

fn parse_partitioned_manifest(
    bytes: &[u8],
) -> Result<PartitionedPublishedGenerationV1, CodeIndexProductionErrorV1> {
    let raw: PartitionedRawEnvelopeV1 = hotpath::measure_block!(
        "code_index.restore.manifest_envelope_parse",
        serde_json::from_slice(bytes).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation manifest decoding failed: {error}"
            ))
        })
    )?;
    let actual_digest = hotpath::measure_block!(
        "code_index.restore.manifest_digest",
        ManifestDigest::from_sha256_bytes(&Sha256::digest(raw.generation.get().as_bytes()))
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
    )?;
    if actual_digest != raw.state_digest {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation manifest state digest does not match its payload".to_owned(),
        ));
    }
    let probe: PartitionedFormatProbeV1 = hotpath::measure_block!(
        "code_index.restore.manifest_revision_probe",
        serde_json::from_str(raw.generation.get()).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation manifest format probe failed: {error}"
            ))
        })
    )?;
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
    let generation: PartitionedPublishedGenerationV1 = hotpath::measure_block!(
        "code_index.restore.manifest_payload_parse",
        serde_json::from_str(raw.generation.get()).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation manifest payload decoding failed: {error}"
            ))
        })
    )?;
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
    Ok(generation)
}

fn snapshot_file_keys<'a>(
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
        hotpath::measure_block!(
            "code_index.restore.segment_read",
            read_segment(descriptor, buffer)
        )?;
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
    collect_bounded_ordered(&window, |(descriptor, segment), _worker| {
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

/// Reads one sealed segment's bytes into the buffer it is handed.
pub type SealedGenerationSegmentReaderV1<'a> = dyn FnMut(SealedGenerationSegmentReadV1<'_>, &mut Vec<u8>) -> Result<(), CodeIndexProductionErrorV1>
    + 'a;

/// Files one window of a sealed generation's segments decodes at a time.
const FILE_WINDOW_FILES_PER_WORKER_V1: usize = 4;

/// A sealed generation's file segments, decoded back one bounded window of
/// files at a time without assembling the generation.
///
/// Only the authenticated partitioned manifest is resident: the snapshot and
/// the ordered segment descriptors. Every window's segments are verified
/// against their content addresses as they decode, and a window's decoded
/// rows are the caller's to drop before the next window is read.
pub struct SealedGenerationFileWindowsV1 {
    generation: PartitionedPublishedGenerationV1,
    scope: FileScopeIdentityV1,
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
        let scope = FileScopeIdentityV1::of(&generation.manifest, &generation.snapshot);
        Ok(Self { generation, scope })
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

    /// Decodes every file segment in manifest order, handing each window's
    /// files to `visit` with the snapshot record each file was sealed from.
    pub(super) fn for_each_file_window<E>(
        &self,
        read_segment: &mut SealedGenerationSegmentReaderV1<'_>,
        mut visit: impl FnMut(
            Vec<(&SanitizedCodeFileV1, PersistedFileGenerationArtifactsV1)>,
        ) -> Result<(), E>,
    ) -> Result<(), E>
    where
        E: From<CodeIndexProductionErrorV1>,
    {
        let descriptors = &self.generation.file_segments;
        let window_files = crate::parallelism::indexing_workers()
            .max(1)
            .saturating_mul(FILE_WINDOW_FILES_PER_WORKER_V1);
        let mut buffers = vec![Vec::new(); window_files];
        let mut start = 0;
        while start < descriptors.len() {
            let pending = &descriptors[start..];
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
            let window = &pending[..read];
            let decoded = decode_segment_window(
                window,
                &buffers[..read],
                &self.generation.manifest.generation_id,
                &self.generation.manifest.snapshot_digest,
                &self.scope,
            )?;
            let files = window
                .iter()
                .zip(decoded)
                .map(|(descriptor, page)| {
                    self.generation
                        .snapshot
                        .files
                        .get(descriptor.file_key as usize)
                        .map(|file| (file, page))
                        .ok_or_else(|| {
                            CodeIndexProductionErrorV1::Contract(
                                "sealed generation file key is outside its snapshot".to_owned(),
                            )
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            start += read;
            visit(files)?;
        }
        Ok(())
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
            let plans = hotpath::measure_block!(
                "code_index.sealed_encode.file_window",
                collect_bounded_ordered(window, |file, _worker| plan_file(file))
            )?;
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
        let generation_evidence = PartitionedSegmentEncoderV1::default()
            .encode_generation_evidence(self, &mut publish_segment)?;
        let statistics = self.generation_statistics()?;
        let generation = PartitionedPublishedGenerationRefV1 {
            format_revision: SEALED_GENERATION_FORMAT_REVISION_V1,
            manifest: &self.manifest,
            snapshot: &self.snapshot,
            statistics: &statistics,
            repository_parse_identity: &self.repository_parse_identity,
            ignored_source_admissions: self.ignored_source_roster.admissions(),
            ignored_source_admissions_digest: self.ignored_source_roster.digest(),
            file_segments: &file_segments,
            coverage: self.coverage,
            capability: &self.capability,
            generation_evidence: &generation_evidence,
        };
        let generation_bytes = hotpath::measure_block!(
            "code_index.sealed_encode.manifest_serialize",
            serde_json::to_vec(&generation).map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation manifest serialization failed: {error}"
                ))
            })
        )?;
        let state_digest = hotpath::measure_block!(
            "code_index.sealed_encode.manifest_digest",
            ManifestDigest::from_sha256_bytes(&Sha256::digest(&generation_bytes))
                .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
        )?;
        hotpath::measure_block!("code_index.sealed_encode.manifest_envelope", {
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
        })
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
    /// path, which no measured hotpath drives through the drain-window
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

    pub fn decode_partitioned_sealed(
        bytes: &[u8],
        mut read_segment: impl FnMut(
            SealedGenerationSegmentReadV1<'_>,
            &mut Vec<u8>,
        ) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let generation = hotpath::measure_block!(
            "code_index.restore.manifest",
            parse_partitioned_manifest(bytes)
        )?;
        let mut files = Vec::with_capacity(generation.file_segments.len());
        // One segment buffer per window slot, reused across windows: the
        // decode holds at most `partitioned_decode_window_files()` segments,
        // each buffer grown only to the largest segment its slot has read.
        let mut buffers = vec![Vec::new(); Self::partitioned_decode_window_files()];
        let scope = FileScopeIdentityV1::of(&generation.manifest, &generation.snapshot);
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
                &scope,
            )?);
        }
        let evidence = hotpath::measure_block!(
            "code_index.restore.generation_evidence",
            decode_generation_evidence(&generation.generation_evidence, read_segment)
        )?;
        let (lineage, projection_request, projection_receipt) =
            hotpath::measure_block!("code_index.restore.evidence_expand", {
                let symbols =
                    occurrence_roster(files.iter().flat_map(|file| file.artifacts.symbols.iter()));
                let chunks = chunk_roster(
                    files
                        .iter()
                        .flat_map(|file| file.artifacts.chunks.chunks.iter()),
                );
                let request = evidence.projection_request.expand(&chunks)?;
                let receipt = evidence.projection_receipt.expand(&request)?;
                let lineage = evidence
                    .lineage
                    .expand(&generation.manifest.generation_id, &symbols)?;
                Ok::<_, CodeIndexProductionErrorV1>((lineage, request, receipt))
            })?;
        assemble_published_generation(StreamingPersistedPublishedGenerationV1 {
            manifest: generation.manifest,
            snapshot: generation.snapshot,
            repository_parse_identity: generation.repository_parse_identity,
            ignored_source_admissions: generation.ignored_source_admissions,
            ignored_source_admissions_digest: generation.ignored_source_admissions_digest,
            files,
            lineage,
            coverage: generation.coverage,
            capability: generation.capability,
            projection_request,
            projection_receipt,
        })
    }

    /// Authenticate only the tiny partitioned manifest and return the metadata
    /// needed to bind already-published text and graph owners. Segment bytes
    /// remain untouched; callers may use this only when those owners already
    /// have their own verified durable artifacts.
    pub fn partitioned_text_metadata(
        bytes: &[u8],
    ) -> Result<VerifiedSealedTextGenerationMetadataV1, CodeIndexProductionErrorV1> {
        let generation = parse_partitioned_manifest(bytes)?;
        VerifiedSealedTextGenerationMetadataV1::from_partitioned_manifest(
            generation.manifest,
            generation.snapshot,
            generation.statistics,
        )
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
        identities.push(SealedGenerationSegmentIdentityV1 {
            digest: generation.generation_evidence.segment_digest,
            size_bytes: generation.generation_evidence.segment_size_bytes,
        });
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
        let mut identities = Vec::with_capacity(generation.file_segments.len().saturating_add(1));
        for segment in generation.file_segments {
            identities.push(SealedGenerationSegmentIdentityV1 {
                digest: segment.segment_digest,
                size_bytes: segment.segment_size_bytes,
            });
        }
        identities.push(SealedGenerationSegmentIdentityV1 {
            digest: generation.generation_evidence.segment_digest,
            size_bytes: generation.generation_evidence.segment_size_bytes,
        });
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
        for descriptor in &generation.file_segments {
            segment.clear();
            read_segment(
                SealedGenerationSegmentReadV1::Whole {
                    digest: &descriptor.segment_digest,
                    size_bytes: descriptor.segment_size_bytes,
                },
                &mut segment,
            )?;
            verify_segment_identity(
                &segment,
                &descriptor.segment_digest,
                descriptor.segment_size_bytes,
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
        evidence.finish()
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
                        { "file_occurrence_id": FIXTURE_FILE, "disposition": "present" },
                        { "file_occurrence_id": "file.partitioned.missing", "disposition": "present" }
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
                SealedGenerationSegmentPublicationV1::File { .. } => {
                    panic!("evidence writer cannot publish a file segment")
                }
            }
            Ok(())
        };
        let mut writer = PartitionedEvidencePageWriterV1::new(&mut publish);
        serde_json::to_writer(&mut writer, &evidence).expect("paged evidence encode");
        let descriptor = writer.finish().expect("paged evidence finish");
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
                SealedGenerationSegmentPublicationV1::File { .. } => {
                    panic!("evidence writer cannot publish a file segment")
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
                SealedGenerationSegmentPublicationV1::File { .. } => {
                    panic!("evidence writer cannot publish a file segment")
                }
            }
            Ok(())
        };
        let mut writer = PartitionedEvidencePageWriterV1::new(&mut publish);
        serde_json::to_writer(&mut writer, &evidence).expect("large paged evidence encode");
        let descriptor = writer.finish().expect("large paged evidence finish");
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
            "the live encoder gauge must include exactly one page and its descriptors"
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
        let Err(error) = parse_partitioned_manifest(&manifest) else {
            panic!("a retired manifest revision must be refused, never migrated")
        };

        assert!(
            matches!(
                error,
                CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(7)
            ),
            "unexpected error: {error}"
        );
        assert!(
            error.to_string().contains("will be rebuilt from source"),
            "a retired revision must tell the operator it rebuilds: {error}"
        );
    }
}
