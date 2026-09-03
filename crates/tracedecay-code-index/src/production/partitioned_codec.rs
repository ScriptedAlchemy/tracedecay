use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::io::{Read, Seek};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use tracedecay_domain::{FileOccurrenceId, ManifestDigest, SymbolOccurrenceId};

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

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartitionedFileSegmentV1 {
    format_revision: u32,
    file: Value,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PartitionedGenerationEvidenceV1 {
    lineage: Vec<SymbolLineageCandidateV1>,
    projection_request: ProjectionBatchRequestV1,
    projection_receipt: ProjectionBatchReceiptV1,
}

#[derive(Clone, Copy)]
enum IdentityFieldV1 {
    Other,
    Generation,
    FileOccurrence,
    SymbolOccurrence,
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

fn collect_symbol_occurrences(
    value: &Value,
    field: IdentityFieldV1,
    occurrences: &mut BTreeSet<String>,
) {
    match value {
        Value::String(value) if matches!(field, IdentityFieldV1::SymbolOccurrence) => {
            occurrences.insert(value.clone());
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
    symbol_keys: &BTreeMap<String, u32>,
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
                if let Some(key) = symbol_keys.get(identity) {
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

fn restore_identity_fields(
    value: &mut Value,
    field: IdentityFieldV1,
    generation_id: &str,
    file_occurrence_id: &str,
    symbol_occurrences: &[SymbolOccurrenceId],
) -> Result<(), CodeIndexProductionErrorV1> {
    match value {
        Value::String(identity) => match field {
            IdentityFieldV1::Generation if identity == GENERATION_ID_MARKER => {
                *identity = generation_id.to_owned();
            }
            IdentityFieldV1::FileOccurrence if identity == FILE_OCCURRENCE_ID_MARKER => {
                *identity = file_occurrence_id.to_owned();
            }
            IdentityFieldV1::SymbolOccurrence
                if identity.starts_with(SYMBOL_OCCURRENCE_ID_MARKER_PREFIX) =>
            {
                let key = identity
                    .strip_prefix(SYMBOL_OCCURRENCE_ID_MARKER_PREFIX)
                    .and_then(|key| key.parse::<usize>().ok())
                    .and_then(|key| symbol_occurrences.get(key))
                    .ok_or_else(|| {
                        CodeIndexProductionErrorV1::Contract(
                            "sealed file segment contains an invalid symbol identity key"
                                .to_owned(),
                        )
                    })?;
                *identity = key.as_str().to_owned();
            }
            IdentityFieldV1::Other
            | IdentityFieldV1::Generation
            | IdentityFieldV1::FileOccurrence
            | IdentityFieldV1::SymbolOccurrence => {}
        },
        Value::Array(values) => {
            for value in values {
                restore_identity_fields(
                    value,
                    field,
                    generation_id,
                    file_occurrence_id,
                    symbol_occurrences,
                )?;
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                restore_identity_fields(
                    value,
                    identity_field(key),
                    generation_id,
                    file_occurrence_id,
                    symbol_occurrences,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn normalize_evidence_identities(
    value: &mut Value,
    key: Option<&str>,
    generation_id: &str,
    symbol_markers: &BTreeMap<String, String>,
    chunk_markers: &BTreeMap<String, String>,
) {
    match value {
        Value::String(identity) => {
            if key.is_some_and(generation_identity_field) && identity == generation_id {
                *identity = GENERATION_ID_MARKER.to_owned();
            } else if key.is_some_and(symbol_identity_field) {
                if let Some(marker) = symbol_markers.get(identity) {
                    *identity = marker.clone();
                }
            } else if key.is_some_and(chunk_identity_field)
                && let Some(marker) = chunk_markers.get(identity)
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

fn restore_evidence_identities(
    value: &mut Value,
    key: Option<&str>,
    generation_id: &str,
    symbol_identities: &BTreeMap<String, String>,
    chunk_identities: &BTreeMap<String, String>,
) -> Result<(), CodeIndexProductionErrorV1> {
    match value {
        Value::String(identity) => {
            if key.is_some_and(generation_identity_field) && identity == GENERATION_ID_MARKER {
                *identity = generation_id.to_owned();
            } else if key.is_some_and(symbol_identity_field)
                && identity.starts_with(SYMBOL_OCCURRENCE_ID_MARKER_PREFIX)
            {
                *identity = symbol_identities.get(identity).cloned().ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed generation evidence contains an invalid symbol key".to_owned(),
                    )
                })?;
            } else if key.is_some_and(chunk_identity_field)
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
                restore_evidence_identities(
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
                restore_evidence_identities(
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

fn encode_file_segment(
    generation_id: &CodeGenerationId,
    file: &FileGenerationArtifactsV1,
    file_key: u32,
) -> Result<(PartitionedFileSegmentDescriptorV1, Vec<u8>), CodeIndexProductionErrorV1> {
    let persisted = PersistedFileGenerationArtifactsRefV1 {
        authority: &file.authority,
        extraction: &file.extraction,
        artifacts: &file.artifacts,
    };
    let mut value = serde_json::to_value(persisted).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed file segment serialization failed: {error}"
        ))
    })?;
    let file_occurrence_id = file.extraction.file_occurrence_id.clone();
    let mut occurrences = BTreeSet::new();
    collect_symbol_occurrences(&value, IdentityFieldV1::Other, &mut occurrences);
    let mut stable_symbols = file
        .artifacts
        .symbols
        .iter()
        .map(|symbol| {
            (
                symbol.identity.as_str().to_owned(),
                symbol.occurrence.as_str().to_owned(),
            )
        })
        .collect::<Vec<_>>();
    stable_symbols.sort();
    let mut ordered_occurrences = stable_symbols
        .into_iter()
        .map(|(_, occurrence)| occurrence)
        .collect::<Vec<_>>();
    let known_occurrences = ordered_occurrences.iter().cloned().collect::<BTreeSet<_>>();
    ordered_occurrences.extend(
        occurrences
            .into_iter()
            .filter(|occurrence| !known_occurrences.contains(occurrence)),
    );
    let symbol_occurrences = ordered_occurrences
        .iter()
        .map(|occurrence| {
            SymbolOccurrenceId::new(occurrence.clone())
                .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let symbol_keys = ordered_occurrences
        .into_iter()
        .enumerate()
        .map(|(key, occurrence)| {
            u32::try_from(key)
                .map(|key| (occurrence, key))
                .map_err(|_| {
                    CodeIndexProductionErrorV1::Contract(
                        "sealed file segment symbol key exceeds u32".to_owned(),
                    )
                })
        })
        .collect::<Result<BTreeMap<_, _>, _>>()?;
    normalize_identity_fields(
        &mut value,
        IdentityFieldV1::Other,
        generation_id.as_str(),
        file_occurrence_id.as_str(),
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
            rows.sort_by_key(Value::to_string);
        }
    }
    let bytes = serde_json::to_vec(&PartitionedFileSegmentV1 {
        format_revision: FILE_SEGMENT_FORMAT_REVISION_V1,
        file: value,
    })
    .map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed file segment serialization failed: {error}"
        ))
    })?;
    let segment_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(&bytes))
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
    let segment_size_bytes = u64::try_from(bytes.len()).map_err(|_| {
        CodeIndexProductionErrorV1::Contract("sealed file segment length exceeds u64".to_owned())
    })?;
    Ok((
        PartitionedFileSegmentDescriptorV1 {
            file_key,
            segment_digest,
            segment_size_bytes,
            file_occurrence_id,
            symbol_occurrences,
        },
        bytes,
    ))
}

fn decode_file_segment(
    descriptor: &PartitionedFileSegmentDescriptorV1,
    generation_id: &CodeGenerationId,
    bytes: &[u8],
) -> Result<PersistedFileGenerationArtifactsV1, CodeIndexProductionErrorV1> {
    let actual_size = u64::try_from(bytes.len()).map_err(|_| {
        CodeIndexProductionErrorV1::Contract("sealed file segment length exceeds u64".to_owned())
    })?;
    if actual_size != descriptor.segment_size_bytes {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed file segment byte size does not match its manifest".to_owned(),
        ));
    }
    let actual_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes))
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
    if actual_digest != descriptor.segment_digest {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed file segment digest does not match its manifest".to_owned(),
        ));
    }
    let mut segment: PartitionedFileSegmentV1 = serde_json::from_slice(bytes).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed file segment decoding failed: {error}"
        ))
    })?;
    if segment.format_revision != FILE_SEGMENT_FORMAT_REVISION_V1 {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed file segment format revision is incompatible".to_owned(),
        ));
    }
    restore_identity_fields(
        &mut segment.file,
        IdentityFieldV1::Other,
        generation_id.as_str(),
        descriptor.file_occurrence_id.as_str(),
        &descriptor.symbol_occurrences,
    )?;
    let mut file: PersistedFileGenerationArtifactsV1 = serde_json::from_value(segment.file)
        .map_err(|error| {
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

fn encode_generation_evidence(
    generation: &CodeIndexPublishedGenerationV1,
    file_segments: &[PartitionedFileSegmentDescriptorV1],
) -> Result<(PartitionedComponentDescriptorV1, Vec<u8>), CodeIndexProductionErrorV1> {
    let mut evidence = serde_json::to_value(PartitionedGenerationEvidenceV1 {
        lineage: generation.lineage.clone(),
        projection_request: generation.projection.request().clone(),
        projection_receipt: generation.projection.receipt().clone(),
    })
    .map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation evidence serialization failed: {error}"
        ))
    })?;
    let mut symbol_markers = BTreeMap::new();
    let mut chunk_markers = BTreeMap::new();
    for descriptor in file_segments {
        for (symbol_key, occurrence) in descriptor.symbol_occurrences.iter().enumerate() {
            symbol_markers.insert(
                occurrence.as_str().to_owned(),
                format!(
                    "{SYMBOL_OCCURRENCE_ID_MARKER_PREFIX}{}:{symbol_key}",
                    descriptor.file_key
                ),
            );
        }
        let file = generation
            .files
            .iter()
            .find(|file| file.extraction.file_occurrence_id == descriptor.file_occurrence_id)
            .ok_or_else(|| {
                CodeIndexProductionErrorV1::Contract(
                    "sealed evidence file is absent from its generation".to_owned(),
                )
            })?;
        for (chunk_key, chunk) in file.artifacts.chunks.chunks.iter().enumerate() {
            chunk_markers.insert(
                chunk.id.as_str().to_owned(),
                format!(
                    "{CHUNK_ID_MARKER_PREFIX}{}:{chunk_key}",
                    descriptor.file_key
                ),
            );
        }
    }
    normalize_evidence_identities(
        &mut evidence,
        None,
        generation.manifest.generation_id.as_str(),
        &symbol_markers,
        &chunk_markers,
    );
    let bytes = serde_json::to_vec(&evidence).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation evidence serialization failed: {error}"
        ))
    })?;
    let segment_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(&bytes))
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
    let segment_size_bytes = u64::try_from(bytes.len()).map_err(|_| {
        CodeIndexProductionErrorV1::Contract(
            "sealed generation evidence length exceeds u64".to_owned(),
        )
    })?;
    Ok((
        PartitionedComponentDescriptorV1 {
            segment_digest,
            segment_size_bytes,
        },
        bytes,
    ))
}

fn decode_generation_evidence(
    descriptor: &PartitionedComponentDescriptorV1,
    bytes: &[u8],
    generation_id: &CodeGenerationId,
    file_segments: &[PartitionedFileSegmentDescriptorV1],
    files: &[PersistedFileGenerationArtifactsV1],
) -> Result<PartitionedGenerationEvidenceV1, CodeIndexProductionErrorV1> {
    let actual_size = u64::try_from(bytes.len()).map_err(|_| {
        CodeIndexProductionErrorV1::Contract(
            "sealed generation evidence length exceeds u64".to_owned(),
        )
    })?;
    if actual_size != descriptor.segment_size_bytes {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation evidence byte size does not match its manifest".to_owned(),
        ));
    }
    let actual_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(bytes))
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
    if actual_digest != descriptor.segment_digest {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation evidence digest does not match its manifest".to_owned(),
        ));
    }
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
    restore_evidence_identities(
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

impl<R: Read + Seek> VerifiedSealedLexicalPageSourceV1<R> {
    pub fn open_partitioned_sealed(
        reader: R,
        manifest_bytes: &[u8],
        source_state_digest: ManifestDigest,
        mut read_segment: impl FnMut(
            &ManifestDigest,
            u64,
        ) -> Result<Vec<u8>, CodeIndexProductionErrorV1>,
        maximum_page_chunks: usize,
        maximum_page_bytes: usize,
    ) -> Result<Option<Self>, CodeIndexProductionErrorV1> {
        let Some(generation) = parse_partitioned_manifest(manifest_bytes)? else {
            return Ok(None);
        };
        let mut files = Vec::with_capacity(generation.file_segments.len());
        for descriptor in &generation.file_segments {
            let bytes = read_segment(&descriptor.segment_digest, descriptor.segment_size_bytes)?;
            files.push(decode_file_segment(
                descriptor,
                &generation.manifest.generation_id,
                &bytes,
            )?);
        }
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
                    let symbol_occurrences = prior_descriptor
                        .symbol_occurrences
                        .iter()
                        .map(|occurrence| {
                            crate::chunks::rematerialized_symbol_occurrence_id(
                                &self.manifest.generation_id,
                                &file.extraction.file_occurrence_id,
                                occurrence,
                            )
                        })
                        .collect::<Result<Vec<_>, _>>()
                        .ok()?;
                    let rematerialized = symbol_occurrences.iter().collect::<BTreeSet<_>>();
                    file.artifacts
                        .symbols
                        .iter()
                        .all(|symbol| rematerialized.contains(&symbol.occurrence))
                        .then(|| PartitionedFileSegmentDescriptorV1 {
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
            let (descriptor, bytes) = encode_file_segment(&self.manifest.generation_id, file, key)?;
            publish_segment(
                SealedGenerationSegmentKindV1::File,
                &descriptor.segment_digest,
                &bytes,
            )?;
            file_segments.push(descriptor);
        }
        file_segments.sort_by_key(|segment| segment.file_key);
        let (generation_evidence, evidence_bytes) =
            encode_generation_evidence(self, &file_segments)?;
        publish_segment(
            SealedGenerationSegmentKindV1::GenerationEvidence,
            &generation_evidence.segment_digest,
            &evidence_bytes,
        )?;
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
        ) -> Result<Vec<u8>, CodeIndexProductionErrorV1>,
    ) -> Result<Option<Self>, CodeIndexProductionErrorV1> {
        let Some(generation) = parse_partitioned_manifest(bytes)? else {
            return Ok(None);
        };
        let mut files = Vec::with_capacity(generation.file_segments.len());
        for descriptor in &generation.file_segments {
            let bytes = read_segment(&descriptor.segment_digest, descriptor.segment_size_bytes)?;
            files.push(decode_file_segment(
                descriptor,
                &generation.manifest.generation_id,
                &bytes,
            )?);
        }
        let evidence_bytes = read_segment(
            &generation.generation_evidence.segment_digest,
            generation.generation_evidence.segment_size_bytes,
        )?;
        let evidence = decode_generation_evidence(
            &generation.generation_evidence,
            &evidence_bytes,
            &generation.manifest.generation_id,
            &generation.file_segments,
            &files,
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

    pub fn verify_partitioned_sealed(
        bytes: &[u8],
        mut read_segment: impl FnMut(
            &ManifestDigest,
            u64,
        ) -> Result<Vec<u8>, CodeIndexProductionErrorV1>,
    ) -> Result<bool, CodeIndexProductionErrorV1> {
        let Some(identities) = Self::partitioned_segment_identities(bytes)? else {
            return Ok(false);
        };
        for identity in identities {
            let segment = read_segment(&identity.digest, identity.size_bytes)?;
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
}
