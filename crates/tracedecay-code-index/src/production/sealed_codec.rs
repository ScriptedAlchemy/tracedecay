use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::*;

/// The sealed-generation envelope revision this build writes.
///
/// Every reader that gates on the sealed format — the publication store, the
/// worker probe, and code-generation retention — must gate on this one value.
pub const SEALED_GENERATION_FORMAT_REVISION_V1: u32 = 5;
pub const MAX_SEALED_CODE_GENERATION_BYTES_V1: u64 = 1024 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedFileGenerationArtifactsV1 {
    authority: ReceiptBoundCodeFileAuthorityV1,
    extraction: ExtractionBatchV1,
    artifacts: CodeFileIndexArtifactsV1,
}

#[derive(Serialize)]
struct PersistedFileGenerationArtifactsRefV1<'a> {
    authority: &'a ReceiptBoundCodeFileAuthorityV1,
    extraction: &'a ExtractionBatchV1,
    artifacts: &'a CodeFileIndexArtifactsV1,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedPublishedGenerationV1 {
    format_revision: CompatibleSealedFormatRevisionV1,
    manifest: CodeGenerationManifestV1,
    snapshot: SanitizedCodeSnapshotV1,
    files: Vec<PersistedFileGenerationArtifactsV1>,
    lineage: Vec<SymbolLineageCandidateV1>,
    coverage: CoverageSummaryV1,
    capability: CodeIndexCapabilityManifestV1,
    projection_request: ProjectionBatchRequestV1,
    projection_receipt: ProjectionBatchReceiptV1,
}

#[derive(Clone, Copy, Debug)]
struct CompatibleSealedFormatRevisionV1;

impl Serialize for CompatibleSealedFormatRevisionV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u32(SEALED_GENERATION_FORMAT_REVISION_V1)
    }
}

impl<'de> Deserialize<'de> for CompatibleSealedFormatRevisionV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if u32::deserialize(deserializer)? != SEALED_GENERATION_FORMAT_REVISION_V1 {
            return Err(serde::de::Error::custom(
                "sealed generation format revision is incompatible",
            ));
        }
        Ok(Self)
    }
}

#[derive(Serialize)]
struct PersistedPublishedGenerationRefV1<'a> {
    format_revision: u32,
    manifest: &'a CodeGenerationManifestV1,
    snapshot: &'a SanitizedCodeSnapshotV1,
    files: Vec<PersistedFileGenerationArtifactsRefV1<'a>>,
    lineage: &'a [SymbolLineageCandidateV1],
    coverage: CoverageSummaryV1,
    capability: &'a CodeIndexCapabilityManifestV1,
    projection_request: &'a ProjectionBatchRequestV1,
    projection_receipt: &'a ProjectionBatchReceiptV1,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SealedPublishedGenerationEnvelopeV1 {
    state_digest: ManifestDigest,
    generation: PersistedPublishedGenerationV1,
}

#[derive(Deserialize)]
struct SealedPublishedGenerationFormatProbeV1 {
    generation: PersistedPublishedGenerationFormatProbeV1,
}

#[derive(Deserialize)]
struct PersistedPublishedGenerationFormatProbeV1 {
    format_revision: u32,
}

#[derive(Serialize)]
struct SealedPublishedGenerationEnvelopeRefV1<'a> {
    state_digest: &'a ManifestDigest,
    generation: PersistedPublishedGenerationRefV1<'a>,
}

fn decode_admitted_json<T: DeserializeOwned, R: std::io::Read>(
    reader: R,
    admitted_len: u64,
) -> Result<T, CodeIndexProductionErrorV1> {
    if admitted_len > MAX_SEALED_CODE_GENERATION_BYTES_V1 {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation exceeds the canonical byte limit".to_owned(),
        ));
    }
    let read_limit = admitted_len.checked_add(1).ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract("sealed generation length overflowed".to_owned())
    })?;
    let mut reader = reader.take(read_limit);
    let decoded = serde_json::from_reader(&mut reader).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!("sealed generation decoding failed: {error}"))
    })?;
    if read_limit - reader.limit() != admitted_len {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation length does not match its admitted length".to_owned(),
        ));
    }
    Ok(decoded)
}

impl CodeIndexPublishedGenerationV1 {
    /// Encode the complete sealed generation for immutable store publication.
    pub fn encode_sealed(&self) -> Result<Vec<u8>, CodeIndexProductionErrorV1> {
        self.validate()?;
        let generation = PersistedPublishedGenerationRefV1 {
            format_revision: SEALED_GENERATION_FORMAT_REVISION_V1,
            manifest: &self.manifest,
            snapshot: &self.snapshot,
            files: self
                .files
                .iter()
                .map(|file| PersistedFileGenerationArtifactsRefV1 {
                    authority: &file.authority,
                    extraction: &file.extraction,
                    artifacts: &file.artifacts,
                })
                .collect(),
            lineage: &self.lineage,
            coverage: self.coverage,
            capability: &self.capability,
            projection_request: self.projection.request(),
            projection_receipt: self.projection.receipt(),
        };
        let state_digest = canonical_sha256(&generation)
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        serde_json::to_vec(&SealedPublishedGenerationEnvelopeRefV1 {
            state_digest: &state_digest,
            generation,
        })
        .map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation serialization failed: {error}"
            ))
        })
    }

    /// Restore and revalidate a complete sealed generation.
    pub fn decode_sealed(bytes: &[u8]) -> Result<Self, CodeIndexProductionErrorV1> {
        let admitted_len = u64::try_from(bytes.len()).map_err(|_| {
            CodeIndexProductionErrorV1::Contract("sealed generation length exceeds u64".to_owned())
        })?;
        Self::decode_sealed_reader(std::io::Cursor::new(bytes), admitted_len)
    }

    pub fn decode_sealed_reader<R: std::io::Read>(
        reader: R,
        admitted_len: u64,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let envelope: SealedPublishedGenerationEnvelopeV1 =
            decode_admitted_json(reader, admitted_len)?;
        let expected_digest = canonical_sha256(&envelope.generation)
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
        if expected_digest != envelope.state_digest {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation state digest does not match its payload".to_owned(),
            ));
        }

        let mut files = Vec::with_capacity(envelope.generation.files.len());
        for file in envelope.generation.files {
            let exact_authority = ExactExtractionAuthorityV1::restore(&file.artifacts.chunks)
                .map_err(CodeIndexProductionErrorV1::Chunk)?;
            files.push(FileGenerationArtifactsV1 {
                authority: file.authority,
                extraction: file.extraction,
                artifacts: file.artifacts,
                exact_authority,
            });
        }
        let chunks = GenerationChunkManifestV1::new(
            envelope.generation.manifest.generation_id.clone(),
            files
                .iter()
                .map(|file| file.artifacts.chunks.clone())
                .collect(),
        )
        .map_err(CodeIndexProductionErrorV1::Increment)?;
        let symbols = GenerationSymbolIndexV1::new(
            envelope.generation.manifest.generation_id.clone(),
            files
                .iter()
                .flat_map(|file| file.artifacts.symbols.clone())
                .collect(),
        )
        .map_err(CodeIndexProductionErrorV1::Lineage)?;
        let imports = derive_import_evidence(&files);
        let (edges, edge_abstentions) = collect_edge_evidence(&files);
        let projection = ProjectionPublicationHandoffV1::restore(
            envelope.generation.projection_request,
            envelope.generation.projection_receipt,
        )
        .map_err(CodeIndexProductionErrorV1::Projection)?;
        let generation = Self {
            manifest: envelope.generation.manifest,
            snapshot: envelope.generation.snapshot,
            files,
            chunks,
            symbols,
            lineage: envelope.generation.lineage,
            imports,
            edges,
            edge_abstentions,
            coverage: envelope.generation.coverage,
            capability: envelope.generation.capability,
            projection,
            validated: OnceLock::new(),
            admitted: OnceLock::new(),
            attribution: OnceLock::new(),
        };
        generation.validate_fresh()?;
        Ok(generation)
    }

    pub fn sealed_format_is_compatible(bytes: &[u8]) -> Result<bool, CodeIndexProductionErrorV1> {
        let probe: SealedPublishedGenerationFormatProbeV1 =
            serde_json::from_slice(bytes).map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation format probe failed: {error}"
                ))
            })?;
        Ok(probe.generation.format_revision == SEALED_GENERATION_FORMAT_REVISION_V1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admitted_json_rejects_extra_and_missing_bytes() {
        let extra = decode_admitted_json::<serde_json::Value, _>(std::io::Cursor::new(b"{} "), 2);
        let missing = decode_admitted_json::<serde_json::Value, _>(std::io::Cursor::new(b"{}"), 3);

        assert!(matches!(
            extra,
            Err(CodeIndexProductionErrorV1::Contract(message))
                if message.contains("admitted length")
        ));
        assert!(matches!(
            missing,
            Err(CodeIndexProductionErrorV1::Contract(message))
                if message.contains("admitted length")
        ));
    }
}
