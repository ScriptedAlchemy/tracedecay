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
//! 4. **`artifacts.symbols` is stably sorted by its `identity` member**, with
//!    a missing or non-string member ordering first.
//! 5. **`artifacts.edges` and `artifacts.unresolved_references` are sorted by
//!    each element's own canonical encoding**, byte-wise — the shipped
//!    comparator was `sort_by_cached_key(Value::to_string)`.
//! 6. Generation evidence is not rewritten. Its page boundaries do not alter
//!    the typed JSON stream, and an aggregate digest authenticates the exact
//!    concatenation.
//!
//! Decoding needs neither rule 1 nor rules 4-6: `serde` accepts any member
//! order and the typed artifacts are re-sorted after restore, so a segment is
//! restored by substituting identities back into the stored bytes and
//! deserializing them directly.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::io::{Read, Seek, Write as IoWrite};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, value::RawValue};
use sha2::{Digest, Sha256};
use tracedecay_domain::{FileOccurrenceId, ManifestDigest, SymbolOccurrenceId};

use super::canonical_json::{
    CanonicalArrayOrderV1, CanonicalPolicyV1, canonicalize_json_into, visit_json_strings,
    write_json_string,
};
use super::sealed_codec::{
    PersistedFileGenerationArtifactsRefV1, PersistedFileGenerationArtifactsV1,
    SEALED_GENERATION_FORMAT_REVISION_V1, StreamingPersistedPublishedGenerationV1,
    StreamingRestoredFilesV1, assemble_published_generation, restore_file_pages,
};
use super::*;

const FILE_SEGMENT_FORMAT_REVISION_V1: u32 = 1;
const GENERATION_ID_MARKER: &str = "$tracedecay:g";
const FILE_OCCURRENCE_ID_MARKER: &str = "$tracedecay:f";
const SYMBOL_OCCURRENCE_ID_MARKER_PREFIX: &str = "$tracedecay:s:";
const CHUNK_ID_MARKER_PREFIX: &str = "$tracedecay:c:";
const GENERATION_EVIDENCE_PAGE_MAX_BYTES_V1: usize = 256 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartitionedFileSegmentDescriptorV1 {
    file_key: u32,
    segment_digest: ManifestDigest,
    segment_size_bytes: u64,
    file_occurrence_id: FileOccurrenceId,
    symbol_occurrences: Vec<SymbolOccurrenceId>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartitionedEvidencePageDescriptorV1 {
    page_ordinal: u32,
    page_digest: ManifestDigest,
    page_size_bytes: u64,
}

#[derive(Clone, Debug, Serialize)]
struct PartitionedGenerationEvidenceDescriptorV1 {
    segment_digest: ManifestDigest,
    segment_size_bytes: u64,
    pages: Vec<PartitionedEvidencePageDescriptorV1>,
    #[serde(skip)]
    legacy_unpaged: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PartitionedGenerationEvidenceDescriptorWireV1 {
    segment_digest: ManifestDigest,
    segment_size_bytes: u64,
    pages: Option<Vec<PartitionedEvidencePageDescriptorV1>>,
}

impl<'de> Deserialize<'de> for PartitionedGenerationEvidenceDescriptorV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = PartitionedGenerationEvidenceDescriptorWireV1::deserialize(deserializer)?;
        let legacy_unpaged = wire.pages.is_none();
        Ok(Self {
            segment_digest: wire.segment_digest,
            segment_size_bytes: wire.segment_size_bytes,
            pages: wire.pages.unwrap_or_default(),
            legacy_unpaged,
        })
    }
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
    format_revision: u32,
    manifest: CodeGenerationManifestV1,
    snapshot: SanitizedCodeSnapshotV1,
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
}

#[derive(Deserialize)]
struct PartitionedFileSegmentIdentityV1 {
    file_key: u32,
    segment_digest: ManifestDigest,
    segment_size_bytes: u64,
    file_occurrence_id: FileOccurrenceId,
}

#[derive(Deserialize)]
struct PartitionedEvidenceSegmentIdentityV1 {
    segment_digest: ManifestDigest,
    segment_size_bytes: u64,
    pages: Option<Vec<PartitionedEvidencePageIdentityV1>>,
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
    #[serde(borrow)]
    file: &'a RawValue,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartitionedGenerationEvidenceV1 {
    lineage: Vec<SymbolLineageCandidateV1>,
    projection_request: ProjectionBatchRequestV1,
    projection_receipt: ProjectionBatchReceiptV1,
}

#[derive(Serialize)]
struct PartitionedGenerationEvidenceRefV1<'a> {
    lineage: &'a [SymbolLineageCandidateV1],
    projection_request: &'a ProjectionBatchRequestV1,
    projection_receipt: &'a ProjectionBatchReceiptV1,
}

#[derive(Clone, Copy)]
enum IdentityFieldV1 {
    Other,
    Generation,
    FileOccurrence,
    SymbolOccurrence,
}

/// The shipped symbol-key assignment: stable symbols order by
/// `(identity, occurrence)` ahead of every remaining serialized occurrence,
/// which orders by occurrence alone.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum SymbolOccurrenceOrderV1<'a> {
    Stable {
        identity: &'a str,
        occurrence: &'a str,
    },
    Remaining(Cow<'a, str>),
}

impl SymbolOccurrenceOrderV1<'_> {
    fn occurrence(&self) -> &str {
        match self {
            Self::Stable { occurrence, .. } => occurrence,
            Self::Remaining(occurrence) => occurrence.as_ref(),
        }
    }
}

fn identity_field(key: &str) -> IdentityFieldV1 {
    match key {
        "generation_id" => IdentityFieldV1::Generation,
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
    file_occurrence_id: &'a str,
    symbol_keys: HashMap<&'a str, u32>,
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
            IdentityFieldV1::FileOccurrence if value == self.file_occurrence_id => {
                write_json_string(FILE_OCCURRENCE_ID_MARKER, out)?;
                Ok(true)
            }
            IdentityFieldV1::SymbolOccurrence => {
                let Some(key) = self.symbol_keys.get(value).copied() else {
                    return Ok(false);
                };
                self.marker.clear();
                self.marker.push_str(SYMBOL_OCCURRENCE_ID_MARKER_PREFIX);
                write!(self.marker, "{key}")
                    .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
                write_json_string(&self.marker, out)?;
                Ok(true)
            }
            IdentityFieldV1::Other
            | IdentityFieldV1::Generation
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
        if path[1] == b"symbols".as_slice() {
            return CanonicalArrayOrderV1::ByStringMember("identity");
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
            legacy_unpaged: false,
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
            self.segment_hasher.update(head);
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
    segment_offset: u64,
    segment_hasher: Sha256,
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
            segment_hasher: Sha256::new(),
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
        let segment_digest = ManifestDigest::from_sha256_bytes(&self.segment_hasher.finalize())
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        if segment_digest != self.descriptor.segment_digest {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation evidence segment digest does not match its manifest".to_owned(),
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
        self.segment_hasher.update(&available[..copied]);
        self.page_offset += copied;
        Ok(copied)
    }
}

/// One generation's reusable segment buffers. Encoding a generation now costs
/// two buffers sized by its largest segment instead of a `serde_json::Value`
/// tree plus a fresh `Vec<u8>` per file.
#[derive(Default)]
struct PartitionedSegmentEncoderV1 {
    payload: Vec<u8>,
    segment: Vec<u8>,
}

impl PartitionedSegmentEncoderV1 {
    fn segment_bytes(&self) -> &[u8] {
        &self.segment
    }

    fn encode_file_segment(
        &mut self,
        generation_id: &CodeGenerationId,
        file: &FileGenerationArtifactsV1,
        file_key: u32,
    ) -> Result<PartitionedFileSegmentDescriptorV1, CodeIndexProductionErrorV1> {
        self.payload.clear();
        serde_json::to_writer(
            &mut self.payload,
            &PersistedFileGenerationArtifactsRefV1 {
                authority: &file.authority,
                extraction: &file.extraction,
                artifacts: &file.artifacts,
            },
        )
        .map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed file segment serialization failed: {error}"
            ))
        })?;
        self.encode_serialized_file_segment(
            generation_id.as_str(),
            file.extraction.file_occurrence_id.clone(),
            file.artifacts
                .symbols
                .iter()
                .map(|symbol| (symbol.identity.as_str(), symbol.occurrence.as_str())),
            file_key,
        )
    }

    /// Rewrite the serialization already staged in `payload` into one canonical
    /// segment. The typed and test entry points share this single authority so
    /// production never carries a second encoder.
    fn encode_serialized_file_segment<'s>(
        &mut self,
        generation_id: &str,
        file_occurrence_id: FileOccurrenceId,
        stable_symbols: impl Iterator<Item = (&'s str, &'s str)>,
        file_key: u32,
    ) -> Result<PartitionedFileSegmentDescriptorV1, CodeIndexProductionErrorV1> {
        let Self { payload, segment } = self;
        // One borrowed ordering authority preserves the shipped assignment:
        // stable symbols sort by (identity, occurrence) first, then every
        // remaining serialized occurrence sorts by occurrence. Deduplication
        // happens before the final identities become the O(1) lookup authority.
        let mut ordered_occurrences = BTreeSet::new();
        ordered_occurrences.extend(stable_symbols.map(|(identity, occurrence)| {
            SymbolOccurrenceOrderV1::Stable {
                identity,
                occurrence,
            }
        }));
        visit_json_strings(
            payload,
            IdentityFieldV1::Other,
            &identity_field,
            &mut |field, value| {
                if matches!(field, IdentityFieldV1::SymbolOccurrence) {
                    ordered_occurrences.insert(SymbolOccurrenceOrderV1::Remaining(value));
                }
                Ok(())
            },
        )?;
        let mut symbol_occurrences = Vec::with_capacity(ordered_occurrences.len());
        let mut known_occurrences = HashSet::with_capacity(ordered_occurrences.len());
        for ordered in &ordered_occurrences {
            let occurrence = ordered.occurrence();
            if !known_occurrences.insert(occurrence) {
                continue;
            }
            let identity = SymbolOccurrenceId::new(occurrence.to_owned())
                .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
            symbol_occurrences.push(identity);
        }
        drop(known_occurrences);
        drop(ordered_occurrences);
        let symbol_keys = symbol_occurrences
            .iter()
            .enumerate()
            .map(|(key, occurrence)| {
                u32::try_from(key)
                    .map(|key| (occurrence.as_str(), key))
                    .map_err(|_| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed file segment symbol key exceeds u32".to_owned(),
                        )
                    })
            })
            .collect::<Result<HashMap<_, _>, _>>()?;
        let mut policy = FileSegmentEncodePolicyV1 {
            generation_id,
            file_occurrence_id: file_occurrence_id.as_str(),
            symbol_keys,
            marker: String::new(),
        };
        segment.clear();
        segment.extend_from_slice(b"{\"format_revision\":");
        serde_json::to_writer(&mut *segment, &FILE_SEGMENT_FORMAT_REVISION_V1).map_err(
            |error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed file segment serialization failed: {error}"
                ))
            },
        )?;
        segment.extend_from_slice(b",\"file\":");
        canonicalize_json_into(payload, &mut policy, segment)?;
        segment.push(b'}');
        let segment_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(&*segment))
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        let segment_size_bytes = u64::try_from(segment.len()).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed file segment length exceeds u64".to_owned(),
            )
        })?;
        Ok(PartitionedFileSegmentDescriptorV1 {
            file_key,
            segment_digest,
            segment_size_bytes,
            file_occurrence_id,
            symbol_occurrences,
        })
    }

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
        let mut writer = PartitionedEvidencePageWriterV1::new(&mut publish);
        let encoded = serde_json::to_writer(
            &mut writer,
            &PartitionedGenerationEvidenceRefV1 {
                lineage: &generation.lineage,
                projection_request: generation.projection.request(),
                projection_receipt: generation.projection.receipt(),
            },
        );
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
    bytes: &[u8],
    restored: &mut Vec<u8>,
) -> Result<PersistedFileGenerationArtifactsV1, CodeIndexProductionErrorV1> {
    verify_segment_identity(
        bytes,
        &descriptor.segment_digest,
        descriptor.segment_size_bytes,
        "sealed file segment length exceeds u64",
        "sealed file segment byte size does not match its manifest",
        "sealed file segment digest does not match its manifest",
    )?;
    let segment: PartitionedRawFileSegmentV1 = serde_json::from_slice(bytes).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed file segment decoding failed: {error}"
        ))
    })?;
    if segment.format_revision != FILE_SEGMENT_FORMAT_REVISION_V1 {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed file segment format revision is incompatible".to_owned(),
        ));
    }
    let mut policy = FileSegmentDecodePolicyV1 {
        generation_id: generation_id.as_str(),
        file_occurrence_id: descriptor.file_occurrence_id.as_str(),
        symbol_occurrences: &descriptor.symbol_occurrences,
    };
    restored.clear();
    canonicalize_json_into(segment.file.get().as_bytes(), &mut policy, restored)?;
    let mut file: PersistedFileGenerationArtifactsV1 =
        serde_json::from_slice(restored).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed file segment payload decoding failed: {error}"
            ))
        })?;
    file.artifacts
        .symbols
        .sort_by(|left, right| left.occurrence.cmp(&right.occurrence));
    file.artifacts.edges.sort_by(|left, right| {
        crate::chunks::canonical_edge_key(left).cmp(&crate::chunks::canonical_edge_key(right))
    });
    file.artifacts.unresolved_references.sort();
    Ok(file)
}

fn legacy_generation_identity_field(key: &str) -> bool {
    matches!(
        key,
        "generation_id"
            | "from_generation"
            | "to_generation"
            | "prior_generation"
            | "source_generation"
    )
}

fn legacy_symbol_identity_field(key: &str) -> bool {
    matches!(
        key,
        "occurrence"
            | "from_occurrence"
            | "to_occurrence"
            | "prior_occurrence"
            | "current_occurrence"
            | "alternatives"
            | "symbol_occurrence_id"
            | "symbol_occurrence_ids"
    )
}

fn legacy_chunk_identity_field(key: &str) -> bool {
    matches!(key, "chunk_id" | "chunk_ids" | "parent_chunk_id")
}

fn restore_legacy_evidence_identities(
    value: &mut Value,
    key: Option<&str>,
    generation_id: &str,
    symbol_identities: &BTreeMap<String, String>,
    chunk_identities: &BTreeMap<String, String>,
) -> Result<(), CodeIndexProductionErrorV1> {
    match value {
        Value::String(identity) => {
            if key.is_some_and(legacy_generation_identity_field) && identity == GENERATION_ID_MARKER
            {
                *identity = generation_id.to_owned();
            } else if key.is_some_and(legacy_symbol_identity_field)
                && identity.starts_with(SYMBOL_OCCURRENCE_ID_MARKER_PREFIX)
            {
                *identity = symbol_identities.get(identity).cloned().ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed generation evidence contains an invalid symbol key".to_owned(),
                    )
                })?;
            } else if key.is_some_and(legacy_chunk_identity_field)
                && identity.starts_with(CHUNK_ID_MARKER_PREFIX)
            {
                *identity = chunk_identities.get(identity).cloned().ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed generation evidence contains an invalid chunk key".to_owned(),
                    )
                })?;
            }
        }
        Value::Array(values) => {
            for value in values {
                restore_legacy_evidence_identities(
                    value,
                    key,
                    generation_id,
                    symbol_identities,
                    chunk_identities,
                )?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                restore_legacy_evidence_identities(
                    value,
                    Some(key),
                    generation_id,
                    symbol_identities,
                    chunk_identities,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn decode_legacy_generation_evidence(
    bytes: &[u8],
    generation_id: &CodeGenerationId,
    file_segments: &[PartitionedFileSegmentDescriptorV1],
    files: &[PersistedFileGenerationArtifactsV1],
) -> Result<PartitionedGenerationEvidenceV1, CodeIndexProductionErrorV1> {
    let mut evidence: Value = serde_json::from_slice(bytes).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation evidence decoding failed: {error}"
        ))
    })?;
    let mut symbol_identities = BTreeMap::new();
    let mut chunk_identities = BTreeMap::new();
    for descriptor in file_segments {
        for (symbol_key, occurrence) in descriptor.symbol_occurrences.iter().enumerate() {
            symbol_identities.insert(
                format!(
                    "{SYMBOL_OCCURRENCE_ID_MARKER_PREFIX}{}:{symbol_key}",
                    descriptor.file_key
                ),
                occurrence.as_str().to_owned(),
            );
        }
        let file = files.get(descriptor.file_key as usize).ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "sealed evidence file key is outside its generation".to_owned(),
            )
        })?;
        for (chunk_key, chunk) in file.artifacts.chunks.chunks.iter().enumerate() {
            chunk_identities.insert(
                format!(
                    "{CHUNK_ID_MARKER_PREFIX}{}:{chunk_key}",
                    descriptor.file_key
                ),
                chunk.id.as_str().to_owned(),
            );
        }
    }
    restore_legacy_evidence_identities(
        &mut evidence,
        None,
        generation_id.as_str(),
        &symbol_identities,
        &chunk_identities,
    )?;
    serde_json::from_value(evidence).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation evidence payload decoding failed: {error}"
        ))
    })
}

fn decode_generation_evidence(
    descriptor: &PartitionedGenerationEvidenceDescriptorV1,
    generation_id: &CodeGenerationId,
    file_segments: &[PartitionedFileSegmentDescriptorV1],
    files: &[PersistedFileGenerationArtifactsV1],
    mut read_segment: impl FnMut(
        SealedGenerationSegmentReadV1<'_>,
        &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1>,
) -> Result<PartitionedGenerationEvidenceV1, CodeIndexProductionErrorV1> {
    if descriptor.legacy_unpaged {
        let mut bytes = Vec::new();
        read_segment(
            SealedGenerationSegmentReadV1::Whole {
                digest: &descriptor.segment_digest,
                size_bytes: descriptor.segment_size_bytes,
            },
            &mut bytes,
        )?;
        verify_segment_identity(
            &bytes,
            &descriptor.segment_digest,
            descriptor.segment_size_bytes,
            "sealed generation evidence length exceeds u64",
            "sealed generation evidence byte size does not match its manifest",
            "sealed generation evidence digest does not match its manifest",
        )?;
        return decode_legacy_generation_evidence(
            bytes.as_slice(),
            generation_id,
            file_segments,
            files,
        );
    }
    let mut reader = PartitionedEvidencePageReaderV1::new(descriptor, &mut read_segment);
    let decoded = serde_json::from_reader(&mut reader);
    if let Some(error) = reader.take_read_error() {
        return Err(error);
    }
    let evidence = decoded.map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation evidence payload decoding failed: {error}"
        ))
    })?;
    reader.finish()?;
    Ok(evidence)
}

fn parse_partitioned_manifest(
    bytes: &[u8],
) -> Result<Option<PartitionedPublishedGenerationV1>, CodeIndexProductionErrorV1> {
    let raw: PartitionedRawEnvelopeV1 = serde_json::from_slice(bytes).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation manifest decoding failed: {error}"
        ))
    })?;
    let actual_digest =
        ManifestDigest::from_sha256_bytes(&Sha256::digest(raw.generation.get().as_bytes()))
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
    if actual_digest != raw.state_digest {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation manifest state digest does not match its payload".to_owned(),
        ));
    }
    let probe: PartitionedFormatProbeV1 =
        serde_json::from_str(raw.generation.get()).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation manifest format probe failed: {error}"
            ))
        })?;
    if probe.format_revision != SEALED_GENERATION_FORMAT_REVISION_V1 {
        return Ok(None);
    }
    let generation: PartitionedPublishedGenerationV1 = serde_json::from_str(raw.generation.get())
        .map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation manifest payload decoding failed: {error}"
        ))
    })?;
    if generation.file_segments.len() != generation.snapshot.files.len() {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation segment count does not match its snapshot".to_owned(),
        ));
    }
    for (expected_key, descriptor) in generation.file_segments.iter().enumerate() {
        let expected_key = u32::try_from(expected_key).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation file key exceeds u32".to_owned(),
            )
        })?;
        if descriptor.file_key != expected_key
            || generation.snapshot.files[expected_key as usize].file_occurrence_id
                != descriptor.file_occurrence_id
        {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation file segments are not canonically keyed".to_owned(),
            ));
        }
    }
    if generation.generation_evidence.legacy_unpaged {
        if generation.generation_evidence.segment_size_bytes == 0 {
            return Err(CodeIndexProductionErrorV1::Contract(
                "legacy sealed generation evidence segment is empty".to_owned(),
            ));
        }
    } else {
        if generation.generation_evidence.pages.is_empty() {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation evidence has no pages".to_owned(),
            ));
        }
        let mut evidence_size_bytes = 0_u64;
        for (expected_ordinal, descriptor) in
            generation.generation_evidence.pages.iter().enumerate()
        {
            let expected_ordinal = u32::try_from(expected_ordinal).map_err(|_| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed generation evidence page count exceeds u32".to_owned(),
                )
            })?;
            if descriptor.page_ordinal != expected_ordinal
                || descriptor.page_size_bytes == 0
                || descriptor.page_size_bytes
                    > u64::try_from(GENERATION_EVIDENCE_PAGE_MAX_BYTES_V1).map_err(|_| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed generation evidence page bound exceeds u64".to_owned(),
                        )
                    })?
            {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "sealed generation evidence pages are not canonically bounded and ordered"
                        .to_owned(),
                ));
            }
            evidence_size_bytes = evidence_size_bytes
                .checked_add(descriptor.page_size_bytes)
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed generation evidence segment length exceeds u64".to_owned(),
                    )
                })?;
        }
        if evidence_size_bytes != generation.generation_evidence.segment_size_bytes {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation evidence segment byte size does not match its pages".to_owned(),
            ));
        }
    }
    Ok(Some(generation))
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

impl<R: Read + Seek> VerifiedSealedLexicalPageSourceV1<R> {
    pub fn open_partitioned_sealed(
        reader: R,
        manifest_bytes: &[u8],
        source_state_digest: ManifestDigest,
        mut read_segment: impl FnMut(
            &ManifestDigest,
            u64,
            &mut Vec<u8>,
        ) -> Result<(), CodeIndexProductionErrorV1>,
        maximum_page_chunks: usize,
        maximum_page_bytes: usize,
    ) -> Result<Option<Self>, CodeIndexProductionErrorV1> {
        let Some(generation) = parse_partitioned_manifest(manifest_bytes)? else {
            return Ok(None);
        };
        let mut files = Vec::with_capacity(generation.file_segments.len());
        let mut segment = Vec::new();
        let mut restored = Vec::new();
        for descriptor in &generation.file_segments {
            segment.clear();
            read_segment(
                &descriptor.segment_digest,
                descriptor.segment_size_bytes,
                &mut segment,
            )?;
            files.push(decode_file_segment(
                descriptor,
                &generation.manifest.generation_id,
                &segment,
                &mut restored,
            )?);
        }
        drop(restored);
        let files = restore_file_pages(files)?;
        Self::open_partitioned_parts(
            reader,
            generation.manifest,
            generation.snapshot,
            files,
            source_state_digest,
            maximum_page_chunks,
            maximum_page_bytes,
        )
        .map(Some)
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
        let parent = parent_manifest_bytes
            .map(parse_partitioned_manifest)
            .transpose()?
            .flatten();
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
                    .snapshot
                    .files
                    .iter()
                    .zip(&parent.file_segments)
                    .map(|(file, descriptor)| (&file.file_occurrence_id, (file, descriptor)))
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
        let mut encoder = PartitionedSegmentEncoderV1::default();
        for file in &self.files {
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
            let reused = parent_segments
                .get(&current_snapshot_file.file_occurrence_id)
                .and_then(|(prior_file, prior_descriptor)| {
                    (*prior_file == current_snapshot_file).then_some(())?;
                    // The reuse gate is fail-closed: the prior descriptor's
                    // occurrences, rebound to this generation, must still open
                    // with exactly the symbols this build produced. The
                    // comparison borrows those symbols instead of cloning two
                    // `String`s each, because on a one-file sync this runs for
                    // every unchanged file in the repository.
                    let mut current_symbol_occurrences = file
                        .artifacts
                        .symbols
                        .iter()
                        .map(|symbol| (symbol.identity.as_str(), symbol.occurrence.as_str()))
                        .collect::<Vec<_>>();
                    current_symbol_occurrences.sort_unstable();
                    if prior_descriptor.symbol_occurrences.len() < current_symbol_occurrences.len()
                    {
                        return None;
                    }
                    let mut symbol_occurrences =
                        Vec::with_capacity(prior_descriptor.symbol_occurrences.len());
                    for (index, occurrence) in
                        prior_descriptor.symbol_occurrences.iter().enumerate()
                    {
                        let rebound = crate::chunks::rematerialized_symbol_occurrence_id(
                            &self.manifest.generation_id,
                            &file.extraction.file_occurrence_id,
                            occurrence,
                        )
                        .ok()?;
                        if let Some((_, expected)) = current_symbol_occurrences.get(index)
                            && rebound.as_str() != *expected
                        {
                            return None;
                        }
                        symbol_occurrences.push(rebound);
                    }
                    Some(PartitionedFileSegmentDescriptorV1 {
                        file_key: key,
                        segment_digest: prior_descriptor.segment_digest.clone(),
                        segment_size_bytes: prior_descriptor.segment_size_bytes,
                        file_occurrence_id: file.extraction.file_occurrence_id.clone(),
                        symbol_occurrences,
                    })
                });
            if let Some(descriptor) = reused {
                file_segments.push(descriptor);
                continue;
            }
            let descriptor =
                encoder.encode_file_segment(&self.manifest.generation_id, file, key)?;
            publish_segment(SealedGenerationSegmentPublicationV1::File {
                digest: &descriptor.segment_digest,
                bytes: encoder.segment_bytes(),
            })?;
            file_segments.push(descriptor);
        }
        file_segments.sort_by_key(|segment| segment.file_key);
        let generation_evidence = encoder.encode_generation_evidence(self, &mut publish_segment)?;
        drop(encoder);
        let generation = PartitionedPublishedGenerationRefV1 {
            format_revision: SEALED_GENERATION_FORMAT_REVISION_V1,
            manifest: &self.manifest,
            snapshot: &self.snapshot,
            repository_parse_identity: &self.repository_parse_identity,
            ignored_source_admissions: self.ignored_source_roster.admissions(),
            ignored_source_admissions_digest: self.ignored_source_roster.digest(),
            file_segments: &file_segments,
            coverage: self.coverage,
            capability: &self.capability,
            generation_evidence: &generation_evidence,
        };
        let generation_bytes = serde_json::to_vec(&generation).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation manifest serialization failed: {error}"
            ))
        })?;
        let state_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(&generation_bytes))
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
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

    pub fn decode_partitioned_sealed(
        bytes: &[u8],
        mut read_segment: impl FnMut(
            SealedGenerationSegmentReadV1<'_>,
            &mut Vec<u8>,
        ) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<Option<Self>, CodeIndexProductionErrorV1> {
        let Some(generation) = parse_partitioned_manifest(bytes)? else {
            return Ok(None);
        };
        let mut files = Vec::with_capacity(generation.file_segments.len());
        let mut segment = Vec::new();
        let mut restored = Vec::new();
        for descriptor in &generation.file_segments {
            segment.clear();
            read_segment(
                SealedGenerationSegmentReadV1::Whole {
                    digest: &descriptor.segment_digest,
                    size_bytes: descriptor.segment_size_bytes,
                },
                &mut segment,
            )?;
            files.push(decode_file_segment(
                descriptor,
                &generation.manifest.generation_id,
                &segment,
                &mut restored,
            )?);
        }
        drop(restored);
        drop(segment);
        let evidence = decode_generation_evidence(
            &generation.generation_evidence,
            &generation.manifest.generation_id,
            &generation.file_segments,
            &files,
            read_segment,
        )?;
        assemble_published_generation(StreamingPersistedPublishedGenerationV1 {
            format_revision: super::sealed_codec::CompatibleSealedFormatRevisionV1(
                generation.format_revision,
            ),
            manifest: generation.manifest,
            snapshot: generation.snapshot,
            repository_parse_identity: generation.repository_parse_identity,
            ignored_source_admissions: generation.ignored_source_admissions,
            ignored_source_admissions_digest: generation.ignored_source_admissions_digest,
            files: StreamingRestoredFilesV1 { files },
            lineage: evidence.lineage,
            coverage: generation.coverage,
            capability: generation.capability,
            projection_request: evidence.projection_request,
            projection_receipt: evidence.projection_receipt,
        })
        .map(Some)
    }

    /// Authenticate only the tiny revision-7 manifest and return the metadata
    /// needed to bind already-published text and graph owners. Segment bytes
    /// remain untouched; callers may use this only when those owners already
    /// have their own verified durable artifacts.
    pub fn partitioned_text_metadata(
        bytes: &[u8],
    ) -> Result<Option<VerifiedSealedTextGenerationMetadataV1>, CodeIndexProductionErrorV1> {
        let Some(generation) = parse_partitioned_manifest(bytes)? else {
            return Ok(None);
        };
        VerifiedSealedTextGenerationMetadataV1::from_partitioned_manifest(
            generation.manifest,
            generation.snapshot,
        )
        .map(Some)
    }

    pub fn partitioned_segment_identities(
        bytes: &[u8],
    ) -> Result<Option<Vec<SealedGenerationSegmentIdentityV1>>, CodeIndexProductionErrorV1> {
        let Some(generation) = parse_partitioned_manifest(bytes)? else {
            return Ok(None);
        };
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
        Ok(Some(identities))
    }

    /// Stream only revision-7 segment descriptors from a generation manifest.
    ///
    /// This projection intentionally does not re-materialize the enclosing
    /// manifest. Retention must first authenticate the complete outer file
    /// against its content-addressed name. It must never replace
    /// [`Self::verify_partitioned_sealed`] at a serving boundary.
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
        if generation.file_segments.len() != generation.snapshot.files.len() {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation segment count does not match its snapshot".to_owned(),
            ));
        }
        let mut identities = Vec::with_capacity(generation.file_segments.len().saturating_add(1));
        for (expected_key, segment) in generation.file_segments.into_iter().enumerate() {
            let expected_key = u32::try_from(expected_key).map_err(|_| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed generation file key exceeds u32".to_owned(),
                )
            })?;
            if segment.file_key != expected_key
                || generation.snapshot.files[expected_key as usize].file_occurrence_id
                    != segment.file_occurrence_id
            {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "sealed generation file segments are not canonically keyed".to_owned(),
                ));
            }
            identities.push(SealedGenerationSegmentIdentityV1 {
                digest: segment.segment_digest,
                size_bytes: segment.segment_size_bytes,
            });
        }
        if let Some(pages) = generation.generation_evidence.pages.as_ref() {
            if pages.is_empty() {
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
            for (expected_ordinal, page) in pages.iter().enumerate() {
                let expected_ordinal = u32::try_from(expected_ordinal).map_err(|_| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed generation evidence page count exceeds u32".to_owned(),
                    )
                })?;
                if page.page_ordinal != expected_ordinal
                    || page.page_size_bytes == 0
                    || page.page_size_bytes > page_max
                {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "sealed generation evidence pages are not canonically bounded and ordered"
                            .to_owned(),
                    ));
                }
                evidence_size_bytes = evidence_size_bytes
                    .checked_add(page.page_size_bytes)
                    .ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed generation evidence segment length exceeds u64".to_owned(),
                        )
                    })?;
            }
            if evidence_size_bytes != generation.generation_evidence.segment_size_bytes {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "sealed generation evidence segment byte size does not match its pages"
                        .to_owned(),
                ));
            }
        } else if generation.generation_evidence.segment_size_bytes == 0 {
            return Err(CodeIndexProductionErrorV1::Contract(
                "legacy sealed generation evidence segment is empty".to_owned(),
            ));
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
    ) -> Result<bool, CodeIndexProductionErrorV1> {
        let Some(generation) = parse_partitioned_manifest(bytes)? else {
            return Ok(false);
        };
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
        if generation.generation_evidence.legacy_unpaged {
            segment.clear();
            read_segment(
                SealedGenerationSegmentReadV1::Whole {
                    digest: &generation.generation_evidence.segment_digest,
                    size_bytes: generation.generation_evidence.segment_size_bytes,
                },
                &mut segment,
            )?;
            verify_segment_identity(
                &segment,
                &generation.generation_evidence.segment_digest,
                generation.generation_evidence.segment_size_bytes,
                "sealed generation evidence length exceeds u64",
                "sealed generation evidence byte size does not match its manifest",
                "sealed generation evidence digest does not match its manifest",
            )?;
        } else {
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
        }
        Ok(true)
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
                        { "file_occurrence_id": FIXTURE_FILE },
                        { "file_occurrence_id": "file.partitioned.missing" }
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
    fn snapshot_file_key_index_preserves_canonical_positions() {
        let first = FileOccurrenceId::new("file.partitioned.first").expect("first file identity");
        let second =
            FileOccurrenceId::new("file.partitioned.second").expect("second file identity");

        let keys = snapshot_file_keys([&first, &second].into_iter()).expect("snapshot file keys");

        assert_eq!(keys.get(&first), Some(&0));
        assert_eq!(keys.get(&second), Some(&1));
    }

    /// The replaced `serde_json::Value` encoder. It survives only here, as the
    /// byte-identity authority the streaming writer is measured against;
    /// production carries exactly one encoder.
    mod reference {
        use super::*;

        #[derive(Serialize)]
        pub(super) struct ReferenceFileSegmentV1 {
            pub(super) format_revision: u32,
            pub(super) file: Value,
        }

        fn collect_symbol_occurrences<'a>(
            value: &'a Value,
            field: IdentityFieldV1,
            occurrences: &mut BTreeSet<SymbolOccurrenceOrderV1<'a>>,
        ) {
            match value {
                Value::String(value) if matches!(field, IdentityFieldV1::SymbolOccurrence) => {
                    occurrences.insert(SymbolOccurrenceOrderV1::Remaining(Cow::Borrowed(value)));
                }
                Value::Array(values) => {
                    for value in values {
                        collect_symbol_occurrences(value, field, occurrences);
                    }
                }
                Value::Object(values) => {
                    for (key, value) in values {
                        collect_symbol_occurrences(value, identity_field(key), occurrences);
                    }
                }
                _ => {}
            }
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
            let mut ordered_occurrences = BTreeSet::new();
            ordered_occurrences.extend(stable_symbols.iter().map(|(identity, occurrence)| {
                SymbolOccurrenceOrderV1::Stable {
                    identity: identity.as_str(),
                    occurrence: occurrence.as_str(),
                }
            }));
            collect_symbol_occurrences(&value, IdentityFieldV1::Other, &mut ordered_occurrences);
            let mut symbol_occurrences = Vec::new();
            let mut known_occurrences = HashSet::new();
            for ordered in &ordered_occurrences {
                let occurrence = ordered.occurrence();
                if !known_occurrences.insert(occurrence.to_owned()) {
                    continue;
                }
                symbol_occurrences.push(occurrence.to_owned());
            }
            drop(known_occurrences);
            drop(ordered_occurrences);
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
            if let Some(symbols) = value
                .get_mut("artifacts")
                .and_then(|artifacts| artifacts.get_mut("symbols"))
                .and_then(Value::as_array_mut)
            {
                symbols.sort_by(|left, right| {
                    left.get("identity")
                        .and_then(Value::as_str)
                        .cmp(&right.get("identity").and_then(Value::as_str))
                });
            }
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
                format_revision: FILE_SEGMENT_FORMAT_REVISION_V1,
                file: value,
            })
            .expect("reference segment bytes");
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
            .map(|symbol| (symbol.identity.clone(), symbol.occurrence.clone()))
            .collect()
    }

    fn streamed_file_segment() -> (Vec<u8>, PartitionedFileSegmentDescriptorV1) {
        let payload = fixture_payload();
        let stable = fixture_stable_symbols();
        let mut encoder = PartitionedSegmentEncoderV1::default();
        serde_json::to_writer(&mut encoder.payload, &payload).expect("streamed payload");
        let descriptor = encoder
            .encode_serialized_file_segment(
                FIXTURE_GENERATION,
                FileOccurrenceId::new(FIXTURE_FILE).expect("fixture file identity"),
                stable
                    .iter()
                    .map(|(identity, occurrence)| (identity.as_str(), occurrence.as_str())),
                7,
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

        let (streamed_bytes, descriptor) = streamed_file_segment();

        assert_eq!(
            String::from_utf8(streamed_bytes.clone()).expect("streamed segment is UTF-8"),
            String::from_utf8(reference_bytes.clone()).expect("reference segment is UTF-8"),
            "the streaming writer must reproduce the shipped segment bytes"
        );
        assert_eq!(
            descriptor
                .symbol_occurrences
                .iter()
                .map(|occurrence| occurrence.as_str().to_owned())
                .collect::<Vec<_>>(),
            reference_occurrences,
            "the symbol key assignment must not move"
        );
        assert_eq!(descriptor.file_key, 7);
        assert_eq!(
            descriptor.segment_size_bytes,
            u64::try_from(reference_bytes.len()).expect("reference length"),
        );
        assert_eq!(
            descriptor.segment_digest,
            ManifestDigest::from_sha256_bytes(&Sha256::digest(&reference_bytes))
                .expect("reference digest"),
            "the segment content address must not move"
        );
        assert!(
            streamed_bytes
                .windows(GENERATION_ID_MARKER.len())
                .any(|window| window == GENERATION_ID_MARKER.as_bytes()),
            "the fixture must actually exercise identity substitution"
        );
    }

    #[test]
    fn streaming_file_segment_decode_restores_the_serialized_payload() {
        let (segment_bytes, descriptor) = streamed_file_segment();
        let segment: PartitionedRawFileSegmentV1 =
            serde_json::from_slice(&segment_bytes).expect("streamed segment envelope");
        let mut policy = FileSegmentDecodePolicyV1 {
            generation_id: FIXTURE_GENERATION,
            file_occurrence_id: FIXTURE_FILE,
            symbol_occurrences: &descriptor.symbol_occurrences,
        };
        let mut restored = Vec::new();

        canonicalize_json_into(segment.file.get().as_bytes(), &mut policy, &mut restored)
            .expect("streamed segment restore");

        let restored: Value = serde_json::from_slice(&restored).expect("restored payload");
        let expected = serde_json::to_value(fixture_payload()).expect("fixture payload value");
        assert_eq!(
            restored.pointer("/extraction/generation_id"),
            expected.pointer("/extraction/generation_id"),
        );
        assert_eq!(
            restored.pointer("/artifacts/chunks/chunks/0/symbol_occurrence_ids"),
            expected.pointer("/artifacts/chunks/chunks/0/symbol_occurrence_ids"),
        );
        assert_eq!(
            restored.pointer("/artifacts/unresolved_references"),
            restored.pointer("/artifacts/unresolved_references"),
        );
        assert_eq!(
            restored.pointer("/authority"),
            expected.pointer("/authority"),
            "escaped keys and values must round-trip"
        );
    }

    #[test]
    fn streaming_file_segment_decode_refuses_an_unknown_symbol_key() {
        let mut policy = FileSegmentDecodePolicyV1 {
            generation_id: FIXTURE_GENERATION,
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
}
