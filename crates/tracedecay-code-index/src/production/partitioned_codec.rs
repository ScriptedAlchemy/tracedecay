//! The partitioned generation codec.
//!
//! # Canonical byte rules
//!
//! Both segment payloads are the compact serialization of a transformed
//! `serde_json::Value`, so their canonical bytes obey these rules — the
//! streaming writer in [`super::canonical_json`] reproduces every one of them
//! without materializing the tree:
//!
//! 1. **Object keys are sorted.** This crate does not enable
//!    `serde_json/preserve_order`, so `serde_json::Map` is a `BTreeMap` and
//!    every object inside a payload is emitted in byte-sorted key order, not
//!    in Rust field-declaration order.
//! 2. **The file segment envelope is declaration ordered.** Only the payload
//!    went through a `Value`; the enclosing record is still
//!    `{"format_revision":<u32>,"file":<payload>}`.
//! 3. **Identity strings are substituted in place** by the key that encloses
//!    them (see [`identity_field`] and [`evidence_identity_field`]); the
//!    classification is reset at every object member and inherited through
//!    arrays.
//! 4. **`artifacts.symbols` is stably sorted by its `identity` member**, with
//!    a missing or non-string member ordering first.
//! 5. **`artifacts.edges` and `artifacts.unresolved_references` are sorted by
//!    each element's own canonical encoding**, byte-wise — the shipped
//!    comparator was `sort_by_cached_key(Value::to_string)`.
//! 6. Generation evidence has no reordered array; it is rules 1 and 3 only.
//!
//! Decoding needs neither rule 1 nor rules 4-6: `serde` accepts any member
//! order and the typed artifacts are re-sorted after restore, so a segment is
//! restored by substituting identities back into the stored bytes and
//! deserializing them directly.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt::Write as _;
use std::io::{Read, Seek};

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
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
struct PartitionedComponentDescriptorV1 {
    segment_digest: ManifestDigest,
    segment_size_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedGenerationSegmentIdentityV1 {
    pub digest: ManifestDigest,
    pub size_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SealedGenerationSegmentKindV1 {
    File,
    GenerationEvidence,
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
    generation_evidence: &'a PartitionedComponentDescriptorV1,
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
    generation_evidence: PartitionedComponentDescriptorV1,
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

fn generation_identity_field(key: &str) -> bool {
    matches!(
        key,
        "generation_id"
            | "from_generation"
            | "to_generation"
            | "prior_generation"
            | "source_generation"
    )
}

fn symbol_identity_field(key: &str) -> bool {
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

fn chunk_identity_field(key: &str) -> bool {
    matches!(key, "chunk_id" | "chunk_ids" | "parent_chunk_id")
}

/// Evidence identity classification. The shipped walk keyed on the enclosing
/// object key with three disjoint key sets, so one classification per key is
/// equivalent to the original `if`/`else if` chain.
#[derive(Clone, Copy)]
enum EvidenceIdentityFieldV1 {
    Other,
    Generation,
    Symbol,
    Chunk,
}

fn evidence_identity_field(key: &str) -> EvidenceIdentityFieldV1 {
    if generation_identity_field(key) {
        EvidenceIdentityFieldV1::Generation
    } else if symbol_identity_field(key) {
        EvidenceIdentityFieldV1::Symbol
    } else if chunk_identity_field(key) {
        EvidenceIdentityFieldV1::Chunk
    } else {
        EvidenceIdentityFieldV1::Other
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

/// Substitutes generation-evidence identities with their canonical markers.
///
/// The marker text is formatted on demand from the `(file_key, item_key)` pair
/// rather than pre-rendered per identity: the maps are repository sized while
/// the evidence that actually references them is delta sized, so rendering
/// eagerly allocated one `String` per symbol and per chunk in the generation on
/// every publish.
struct EvidenceEncodePolicyV1<'a> {
    generation_id: &'a str,
    symbol_markers: HashMap<&'a str, (u32, usize)>,
    chunk_markers: HashMap<&'a str, (u32, usize)>,
    marker: String,
}

impl EvidenceEncodePolicyV1<'_> {
    fn render_marker(&mut self, prefix: &str, file_key: u32, item_key: usize) -> &str {
        self.marker.clear();
        self.marker.push_str(prefix);
        // `write!` into a `String` is infallible; the formatter only fails when
        // the sink does.
        let _ = write!(self.marker, "{file_key}:{item_key}");
        &self.marker
    }
}

impl CanonicalPolicyV1 for EvidenceEncodePolicyV1<'_> {
    type Field = EvidenceIdentityFieldV1;

    fn root_field(&self) -> Self::Field {
        EvidenceIdentityFieldV1::Other
    }

    fn field_for_key(&self, key: &str) -> Self::Field {
        evidence_identity_field(key)
    }

    fn rewrite_string(
        &mut self,
        field: Self::Field,
        value: &str,
        out: &mut Vec<u8>,
    ) -> Result<bool, CodeIndexProductionErrorV1> {
        match field {
            EvidenceIdentityFieldV1::Generation if value == self.generation_id => {
                write_json_string(GENERATION_ID_MARKER, out)?;
            }
            EvidenceIdentityFieldV1::Symbol => {
                let Some((file_key, symbol_key)) = self.symbol_markers.get(value).copied() else {
                    return Ok(false);
                };
                let rendered =
                    self.render_marker(SYMBOL_OCCURRENCE_ID_MARKER_PREFIX, file_key, symbol_key);
                write_json_string(rendered, out)?;
            }
            EvidenceIdentityFieldV1::Chunk => {
                let Some((file_key, chunk_key)) = self.chunk_markers.get(value).copied() else {
                    return Ok(false);
                };
                let rendered = self.render_marker(CHUNK_ID_MARKER_PREFIX, file_key, chunk_key);
                write_json_string(rendered, out)?;
            }
            EvidenceIdentityFieldV1::Other | EvidenceIdentityFieldV1::Generation => {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn sorts_object_keys(&self) -> bool {
        true
    }
}

/// Restores generation-evidence identities directly from the segment and file
/// authorities, without first collecting every marker into a side map.
struct EvidenceDecodePolicyV1<'a> {
    generation_id: &'a str,
    file_segments: &'a [PartitionedFileSegmentDescriptorV1],
    files: &'a [PersistedFileGenerationArtifactsV1],
}

impl CanonicalPolicyV1 for EvidenceDecodePolicyV1<'_> {
    type Field = EvidenceIdentityFieldV1;

    fn root_field(&self) -> Self::Field {
        EvidenceIdentityFieldV1::Other
    }

    fn field_for_key(&self, key: &str) -> Self::Field {
        evidence_identity_field(key)
    }

    fn rewrite_string(
        &mut self,
        field: Self::Field,
        value: &str,
        out: &mut Vec<u8>,
    ) -> Result<bool, CodeIndexProductionErrorV1> {
        match field {
            EvidenceIdentityFieldV1::Generation if value == GENERATION_ID_MARKER => {
                write_json_string(self.generation_id, out)?;
                Ok(true)
            }
            EvidenceIdentityFieldV1::Symbol
                if value.starts_with(SYMBOL_OCCURRENCE_ID_MARKER_PREFIX) =>
            {
                const INVALID: &str = "sealed generation evidence contains an invalid symbol key";
                let (file_key, symbol_key) =
                    parse_evidence_marker(value, SYMBOL_OCCURRENCE_ID_MARKER_PREFIX, INVALID)?;
                let occurrence = self
                    .file_segments
                    .get(file_key)
                    .and_then(|descriptor| descriptor.symbol_occurrences.get(symbol_key))
                    .ok_or_else(|| CodeIndexProductionErrorV1::Contract(INVALID.to_owned()))?;
                write_json_string(occurrence.as_str(), out)?;
                Ok(true)
            }
            EvidenceIdentityFieldV1::Chunk if value.starts_with(CHUNK_ID_MARKER_PREFIX) => {
                const INVALID: &str = "sealed generation evidence contains an invalid chunk key";
                let (file_key, chunk_key) =
                    parse_evidence_marker(value, CHUNK_ID_MARKER_PREFIX, INVALID)?;
                let chunk = self
                    .files
                    .get(file_key)
                    .and_then(|file| file.artifacts.chunks.chunks.get(chunk_key))
                    .ok_or_else(|| CodeIndexProductionErrorV1::Contract(INVALID.to_owned()))?;
                write_json_string(chunk.id.as_str(), out)?;
                Ok(true)
            }
            EvidenceIdentityFieldV1::Other
            | EvidenceIdentityFieldV1::Generation
            | EvidenceIdentityFieldV1::Symbol
            | EvidenceIdentityFieldV1::Chunk => Ok(false),
        }
    }

    fn sorts_object_keys(&self) -> bool {
        false
    }
}

fn parse_evidence_marker(
    marker: &str,
    prefix: &str,
    invalid_message: &'static str,
) -> Result<(usize, usize), CodeIndexProductionErrorV1> {
    marker
        .strip_prefix(prefix)
        .and_then(|key| key.split_once(':'))
        .and_then(|(file_key, item_key)| {
            Some((
                file_key.parse::<usize>().ok()?,
                item_key.parse::<usize>().ok()?,
            ))
        })
        .ok_or_else(|| CodeIndexProductionErrorV1::Contract(invalid_message.to_owned()))
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
        file_segments: &[PartitionedFileSegmentDescriptorV1],
    ) -> Result<PartitionedComponentDescriptorV1, CodeIndexProductionErrorV1> {
        let Self { payload, segment } = self;
        payload.clear();
        serde_json::to_writer(
            &mut *payload,
            &PartitionedGenerationEvidenceRefV1 {
                lineage: &generation.lineage,
                projection_request: generation.projection.request(),
                projection_receipt: generation.projection.receipt(),
            },
        )
        .map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation evidence serialization failed: {error}"
            ))
        })?;
        let file_index = generation_file_index(
            generation
                .files
                .iter()
                .map(|file| &file.extraction.file_occurrence_id),
        )?;
        let symbol_capacity = file_segments
            .iter()
            .map(|descriptor| descriptor.symbol_occurrences.len())
            .sum();
        let chunk_capacity = generation
            .files
            .iter()
            .map(|file| file.artifacts.chunks.chunks.len())
            .sum();
        let mut symbol_markers = HashMap::with_capacity(symbol_capacity);
        let mut chunk_markers = HashMap::with_capacity(chunk_capacity);
        for descriptor in file_segments {
            for (symbol_key, occurrence) in descriptor.symbol_occurrences.iter().enumerate() {
                symbol_markers.insert(occurrence.as_str(), (descriptor.file_key, symbol_key));
            }
            let file_key = file_index
                .get(&descriptor.file_occurrence_id)
                .copied()
                .ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed evidence file is absent from its generation".to_owned(),
                    )
                })?;
            let file = generation.files.get(file_key).ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed evidence file is absent from its generation".to_owned(),
                )
            })?;
            for (chunk_key, chunk) in file.artifacts.chunks.chunks.iter().enumerate() {
                chunk_markers.insert(chunk.id.as_str(), (descriptor.file_key, chunk_key));
            }
        }
        let mut policy = EvidenceEncodePolicyV1 {
            generation_id: generation.manifest.generation_id.as_str(),
            symbol_markers,
            chunk_markers,
            marker: String::new(),
        };
        segment.clear();
        canonicalize_json_into(payload, &mut policy, segment)?;
        let segment_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(&*segment))
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        let segment_size_bytes = u64::try_from(segment.len()).map_err(|_| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation evidence length exceeds u64".to_owned(),
            )
        })?;
        Ok(PartitionedComponentDescriptorV1 {
            segment_digest,
            segment_size_bytes,
        })
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

fn decode_generation_evidence(
    descriptor: &PartitionedComponentDescriptorV1,
    bytes: &[u8],
    generation_id: &CodeGenerationId,
    file_segments: &[PartitionedFileSegmentDescriptorV1],
    files: &[PersistedFileGenerationArtifactsV1],
    restored: &mut Vec<u8>,
) -> Result<PartitionedGenerationEvidenceV1, CodeIndexProductionErrorV1> {
    verify_segment_identity(
        bytes,
        &descriptor.segment_digest,
        descriptor.segment_size_bytes,
        "sealed generation evidence length exceeds u64",
        "sealed generation evidence byte size does not match its manifest",
        "sealed generation evidence digest does not match its manifest",
    )?;
    let mut policy = EvidenceDecodePolicyV1 {
        generation_id: generation_id.as_str(),
        file_segments,
        files,
    };
    restored.clear();
    canonicalize_json_into(bytes, &mut policy, restored)?;
    serde_json::from_slice(restored).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation evidence payload decoding failed: {error}"
        ))
    })
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

fn generation_file_index<'a>(
    file_occurrences: impl Iterator<Item = &'a FileOccurrenceId>,
) -> Result<HashMap<&'a FileOccurrenceId, usize>, CodeIndexProductionErrorV1> {
    let mut index = HashMap::new();
    for (file_key, occurrence) in file_occurrences.enumerate() {
        if index.insert(occurrence, file_key).is_some() {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation repeats a file occurrence".to_owned(),
            ));
        }
    }
    Ok(index)
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
        mut publish_segment: impl FnMut(
            &ManifestDigest,
            &[u8],
        ) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<Vec<u8>, CodeIndexProductionErrorV1> {
        self.encode_partitioned_sealed_with_parent(None, |_, digest, bytes| {
            publish_segment(digest, bytes)
        })
    }

    pub fn encode_partitioned_sealed_with_parent(
        &self,
        parent_manifest_bytes: Option<&[u8]>,
        mut publish_segment: impl FnMut(
            SealedGenerationSegmentKindV1,
            &ManifestDigest,
            &[u8],
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
            publish_segment(
                SealedGenerationSegmentKindV1::File,
                &descriptor.segment_digest,
                encoder.segment_bytes(),
            )?;
            file_segments.push(descriptor);
        }
        file_segments.sort_by_key(|segment| segment.file_key);
        let generation_evidence = encoder.encode_generation_evidence(self, &file_segments)?;
        publish_segment(
            SealedGenerationSegmentKindV1::GenerationEvidence,
            &generation_evidence.segment_digest,
            encoder.segment_bytes(),
        )?;
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
            &ManifestDigest,
            u64,
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
        segment.clear();
        read_segment(
            &generation.generation_evidence.segment_digest,
            generation.generation_evidence.segment_size_bytes,
            &mut segment,
        )?;
        let evidence = decode_generation_evidence(
            &generation.generation_evidence,
            &segment,
            &generation.manifest.generation_id,
            &generation.file_segments,
            &files,
            &mut restored,
        )?;
        drop(restored);
        drop(segment);
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

    pub fn verify_partitioned_sealed(
        bytes: &[u8],
        mut read_segment: impl FnMut(
            &ManifestDigest,
            u64,
            &mut Vec<u8>,
        ) -> Result<(), CodeIndexProductionErrorV1>,
    ) -> Result<bool, CodeIndexProductionErrorV1> {
        let Some(identities) = Self::partitioned_segment_identities(bytes)? else {
            return Ok(false);
        };
        let mut segment = Vec::new();
        for identity in identities {
            segment.clear();
            read_segment(&identity.digest, identity.size_bytes, &mut segment)?;
            let actual_size = u64::try_from(segment.len()).map_err(|_| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed generation segment length exceeds u64".to_owned(),
                )
            })?;
            let actual_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(&segment))
                .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
            if actual_size != identity.size_bytes || actual_digest != identity.digest {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "sealed generation segment does not match its content address".to_owned(),
                ));
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    #[test]
    fn snapshot_file_key_index_preserves_canonical_positions() {
        let first = FileOccurrenceId::new("file.partitioned.first").expect("first file identity");
        let second =
            FileOccurrenceId::new("file.partitioned.second").expect("second file identity");

        let keys = snapshot_file_keys([&first, &second].into_iter()).expect("snapshot file keys");

        assert_eq!(keys.get(&first), Some(&0));
        assert_eq!(keys.get(&second), Some(&1));
    }

    #[test]
    fn generation_file_index_maps_occurrences_once() {
        let first = FileOccurrenceId::new("file.partitioned.first").expect("first file identity");
        let second =
            FileOccurrenceId::new("file.partitioned.second").expect("second file identity");

        let index =
            generation_file_index([&first, &second].into_iter()).expect("generation file index");

        assert_eq!(index.get(&first), Some(&0));
        assert_eq!(index.get(&second), Some(&1));
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

        fn normalize_evidence_identities(
            value: &mut Value,
            key: Option<&str>,
            generation_id: &str,
            symbol_markers: &HashMap<&str, String>,
            chunk_markers: &HashMap<&str, String>,
        ) {
            match value {
                Value::String(identity) => {
                    if key.is_some_and(generation_identity_field) && identity == generation_id {
                        *identity = GENERATION_ID_MARKER.to_owned();
                    } else if key.is_some_and(symbol_identity_field) {
                        if let Some(marker) = symbol_markers.get(identity.as_str()) {
                            *identity = marker.clone();
                        }
                    } else if key.is_some_and(chunk_identity_field)
                        && let Some(marker) = chunk_markers.get(identity.as_str())
                    {
                        *identity = marker.clone();
                    }
                }
                Value::Array(values) => {
                    for value in values {
                        normalize_evidence_identities(
                            value,
                            key,
                            generation_id,
                            symbol_markers,
                            chunk_markers,
                        );
                    }
                }
                Value::Object(values) => {
                    for (key, value) in values {
                        normalize_evidence_identities(
                            value,
                            Some(key),
                            generation_id,
                            symbol_markers,
                            chunk_markers,
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

        /// The whole replaced generation-evidence encode.
        pub(super) fn evidence_bytes(
            payload: &impl Serialize,
            generation_id: &str,
            symbol_markers: &HashMap<&str, String>,
            chunk_markers: &HashMap<&str, String>,
        ) -> Vec<u8> {
            let mut evidence = serde_json::to_value(payload).expect("reference evidence value");
            normalize_evidence_identities(
                &mut evidence,
                None,
                generation_id,
                symbol_markers,
                chunk_markers,
            );
            serde_json::to_vec(&evidence).expect("reference evidence bytes")
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

    #[derive(Serialize)]
    struct FixtureEvidence {
        lineage: Vec<FixtureLineage>,
        projection_request: FixtureProjectionRequest,
    }

    #[derive(Serialize)]
    struct FixtureLineage {
        to_occurrence: String,
        from_occurrence: String,
        prior_generation: String,
        source_generation: String,
    }

    #[derive(Serialize)]
    struct FixtureProjectionRequest {
        generation_id: String,
        chunk_ids: Vec<String>,
        parent_chunk_id: Option<String>,
    }

    #[test]
    fn streaming_evidence_bytes_match_the_value_encoder() {
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
        };
        let first = occurrence(1);
        let third = occurrence(3);
        let symbol_keys = HashMap::from([(first.as_str(), (0_u32, 1)), (third.as_str(), (0, 0))]);
        let chunk_keys = HashMap::from([
            ("chunk.partitioned.fixture.00", (0_u32, 0)),
            ("chunk.partitioned.fixture.01", (0, 1)),
        ]);
        // The reference encoder pre-rendered every marker; rendering them here
        // from the same keys keeps the comparison exact.
        fn rendered_markers<'a>(
            keys: &HashMap<&'a str, (u32, usize)>,
            prefix: &str,
        ) -> HashMap<&'a str, String> {
            keys.iter()
                .map(|(identity, (file_key, item_key))| {
                    (*identity, format!("{prefix}{file_key}:{item_key}"))
                })
                .collect()
        }
        let reference_bytes = reference::evidence_bytes(
            &evidence,
            FIXTURE_GENERATION,
            &rendered_markers(&symbol_keys, SYMBOL_OCCURRENCE_ID_MARKER_PREFIX),
            &rendered_markers(&chunk_keys, CHUNK_ID_MARKER_PREFIX),
        );

        let mut payload = Vec::new();
        serde_json::to_writer(&mut payload, &evidence).expect("streamed evidence payload");
        let mut policy = EvidenceEncodePolicyV1 {
            generation_id: FIXTURE_GENERATION,
            symbol_markers: symbol_keys,
            chunk_markers: chunk_keys,
            marker: String::new(),
        };
        let mut streamed = Vec::new();
        canonicalize_json_into(&payload, &mut policy, &mut streamed)
            .expect("streamed evidence encode");

        assert_eq!(
            String::from_utf8(streamed).expect("streamed evidence is UTF-8"),
            String::from_utf8(reference_bytes).expect("reference evidence is UTF-8"),
            "the streaming writer must reproduce the shipped evidence bytes"
        );
    }
}
