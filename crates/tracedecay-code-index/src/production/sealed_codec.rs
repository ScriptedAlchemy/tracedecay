use std::io::{Read, Seek, SeekFrom, Write};

#[cfg(test)]
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

use super::*;

/// The sealed-generation envelope revision this build writes.
///
/// Every reader that gates on the sealed format — the publication store, the
/// worker probe, and code-generation retention — must gate on this one value.
pub(super) const LEGACY_CANONICAL_SEALED_GENERATION_FORMAT_REVISION: u32 = 5;
pub const SEALED_GENERATION_FORMAT_REVISION_V1: u32 = 6;
/// One bound, enforced on both sides of the sealed store: encoding refuses to
/// publish a generation larger than this, and decoding refuses to admit one.
/// The bound previously applied only to reads while publication happily wrote
/// larger envelopes, so a large repository sealed generations (~1.5 GB here)
/// that every later load refused as "corrupt" — permanently denying its own
/// graph. Two GiB admits those real generations while keeping decode memory
/// bounded.
pub const MAX_SEALED_CODE_GENERATION_BYTES_V1: u64 = 2 * 1024 * 1024 * 1024;

fn admit_sealed_generation_len(len: u64) -> Result<(), CodeIndexProductionErrorV1> {
    if len > MAX_SEALED_CODE_GENERATION_BYTES_V1 {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation exceeds the canonical byte limit".to_owned(),
        ));
    }
    Ok(())
}

pub const fn sealed_generation_format_revision_is_compatible(revision: u32) -> bool {
    matches!(
        revision,
        LEGACY_CANONICAL_SEALED_GENERATION_FORMAT_REVISION | SEALED_GENERATION_FORMAT_REVISION_V1
    )
}

pub fn sealed_generation_payload_digest<T: Serialize>(
    format_revision: u32,
    generation: &T,
) -> Result<ManifestDigest, CodeIndexProductionErrorV1> {
    match format_revision {
        LEGACY_CANONICAL_SEALED_GENERATION_FORMAT_REVISION => canonical_sha256(generation)
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string())),
        SEALED_GENERATION_FORMAT_REVISION_V1 => {
            json_generation_bytes_and_digest(generation).map(|(_, digest)| digest)
        }
        _ => Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation format revision is incompatible".to_owned(),
        )),
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PersistedFileGenerationArtifactsV1 {
    pub(super) authority: ReceiptBoundCodeFileAuthorityV1,
    pub(super) extraction: ExtractionBatchV1,
    pub(super) artifacts: CodeFileIndexArtifactsV1,
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
    repository_parse_identity: CodeIndexRepositoryParseIdentityV1,
    ignored_source_admissions: Vec<CodeIndexIgnoredSourceAdmissionV1>,
    ignored_source_admissions_digest: ManifestDigest,
    files: Vec<PersistedFileGenerationArtifactsV1>,
    lineage: Vec<SymbolLineageCandidateV1>,
    coverage: CoverageSummaryV1,
    capability: CodeIndexCapabilityManifestV1,
    projection_request: ProjectionBatchRequestV1,
    projection_receipt: ProjectionBatchReceiptV1,
}

#[derive(Clone, Copy, Debug)]
struct CompatibleSealedFormatRevisionV1(u32);

impl Serialize for CompatibleSealedFormatRevisionV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u32(self.0)
    }
}

impl<'de> Deserialize<'de> for CompatibleSealedFormatRevisionV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let revision = u32::deserialize(deserializer)?;
        if !sealed_generation_format_revision_is_compatible(revision) {
            return Err(serde::de::Error::custom(
                "sealed generation format revision is incompatible",
            ));
        }
        Ok(Self(revision))
    }
}

#[derive(Serialize)]
struct PersistedPublishedGenerationRefV1<'a> {
    format_revision: u32,
    manifest: &'a CodeGenerationManifestV1,
    snapshot: &'a SanitizedCodeSnapshotV1,
    repository_parse_identity: &'a CodeIndexRepositoryParseIdentityV1,
    ignored_source_admissions: &'a [CodeIndexIgnoredSourceAdmissionV1],
    ignored_source_admissions_digest: &'a ManifestDigest,
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
struct SealedPublishedGenerationRawEnvelopeV1 {
    state_digest: ManifestDigest,
    generation: Box<RawValue>,
}

#[derive(Deserialize)]
struct SealedPublishedGenerationFormatProbeV1 {
    generation: PersistedPublishedGenerationFormatProbeV1,
}

#[derive(Deserialize)]
struct PersistedPublishedGenerationFormatProbeV1 {
    format_revision: u32,
}

#[cfg(test)]
fn decode_admitted_json<T: DeserializeOwned, R: std::io::Read>(
    reader: R,
    admitted_len: u64,
) -> Result<T, CodeIndexProductionErrorV1> {
    let bytes = read_admitted_bytes(reader, admitted_len)?;
    serde_json::from_slice(&bytes).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!("sealed generation decoding failed: {error}"))
    })
}

fn read_admitted_bytes<R: std::io::Read>(
    reader: R,
    admitted_len: u64,
) -> Result<Vec<u8>, CodeIndexProductionErrorV1> {
    admit_sealed_generation_len(admitted_len)?;
    let read_limit = admitted_len.checked_add(1).ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract("sealed generation length overflowed".to_owned())
    })?;
    let mut reader = reader.take(read_limit);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!("sealed generation decoding failed: {error}"))
    })?;
    if read_limit - reader.limit() != admitted_len {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation length does not match its admitted length".to_owned(),
        ));
    }
    Ok(bytes)
}

const SEALED_GENERATION_WRITE_CHUNK_BYTES_V1: usize = 64 * 1024;

struct BoundedChunkWriterV1<'a, W> {
    writer: &'a mut W,
    written: u64,
    byte_limit: u64,
    maximum_write: usize,
    limit_exceeded: bool,
}

impl<W: Write> Write for BoundedChunkWriterV1<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let remaining = self.byte_limit.saturating_sub(self.written);
        if remaining == 0 {
            self.limit_exceeded = true;
            return Err(std::io::Error::new(
                std::io::ErrorKind::FileTooLarge,
                "sealed generation exceeds the canonical byte limit",
            ));
        }
        let remaining = usize::try_from(remaining).unwrap_or(usize::MAX);
        let admitted = bytes.len().min(self.maximum_write).min(remaining);
        let written = self.writer.write(&bytes[..admitted])?;
        self.written = self
            .written
            .checked_add(u64::try_from(written).map_err(std::io::Error::other)?)
            .ok_or_else(|| std::io::Error::other("sealed generation length overflowed"))?;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

struct GenerationDigestWriterV1<'writer, 'sink, W> {
    writer: &'writer mut BoundedChunkWriterV1<'sink, W>,
    hasher: Sha256,
}

impl<W: Write> Write for GenerationDigestWriterV1<'_, '_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.writer.write(bytes)?;
        self.hasher.update(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}

fn byte_limit_error() -> CodeIndexProductionErrorV1 {
    CodeIndexProductionErrorV1::Contract(
        "sealed generation exceeds the canonical byte limit".to_owned(),
    )
}

fn write_chunked<W: Write>(
    writer: &mut W,
    mut bytes: &[u8],
    maximum_write: usize,
) -> std::io::Result<()> {
    while !bytes.is_empty() {
        let written = writer.write(&bytes[..bytes.len().min(maximum_write)])?;
        if written == 0 {
            return Err(std::io::ErrorKind::WriteZero.into());
        }
        bytes = &bytes[written..];
    }
    Ok(())
}

fn write_generation_envelope_with_limits<T: Serialize, W: Write + Seek>(
    generation: &T,
    writer: &mut W,
    byte_limit: u64,
    maximum_write: usize,
) -> Result<u64, CodeIndexProductionErrorV1> {
    if maximum_write == 0 {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation write chunk must be non-zero".to_owned(),
        ));
    }
    let placeholder = ManifestDigest::from_sha256_bytes(&[0; 32])
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
    let envelope_start = writer.stream_position().map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation writer position failed: {error}"
        ))
    })?;
    let (digest_start, digest_end, generation_hash, written) = {
        let mut bounded = BoundedChunkWriterV1 {
            writer,
            written: 0,
            byte_limit,
            maximum_write,
            limit_exceeded: false,
        };
        bounded.write_all(b"{\"state_digest\":").map_err(|error| {
            if bounded.limit_exceeded {
                byte_limit_error()
            } else {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation serialization failed: {error}"
                ))
            }
        })?;
        let digest_start = envelope_start.checked_add(bounded.written).ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation writer position overflowed".to_owned(),
            )
        })?;
        if let Err(error) = serde_json::to_writer(&mut bounded, &placeholder) {
            return Err(if bounded.limit_exceeded {
                byte_limit_error()
            } else {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation digest serialization failed: {error}"
                ))
            });
        }
        let digest_end = envelope_start.checked_add(bounded.written).ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation writer position overflowed".to_owned(),
            )
        })?;
        bounded.write_all(b",\"generation\":").map_err(|error| {
            if bounded.limit_exceeded {
                byte_limit_error()
            } else {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation serialization failed: {error}"
                ))
            }
        })?;
        let generation_hash = {
            let mut generation_writer = GenerationDigestWriterV1 {
                writer: &mut bounded,
                hasher: Sha256::new(),
            };
            if let Err(error) = serde_json::to_writer(&mut generation_writer, generation) {
                return Err(if generation_writer.writer.limit_exceeded {
                    byte_limit_error()
                } else {
                    CodeIndexProductionErrorV1::Contract(format!(
                        "sealed generation serialization failed: {error}"
                    ))
                });
            }
            generation_writer.hasher.finalize()
        };
        bounded.write_all(b"}").map_err(|error| {
            if bounded.limit_exceeded {
                byte_limit_error()
            } else {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation serialization failed: {error}"
                ))
            }
        })?;
        (digest_start, digest_end, generation_hash, bounded.written)
    };

    let state_digest = ManifestDigest::from_sha256_bytes(&generation_hash)
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))?;
    let digest_bytes = serde_json::to_vec(&state_digest).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation digest serialization failed: {error}"
        ))
    })?;
    let digest_width = digest_end
        .checked_sub(digest_start)
        .and_then(|width| usize::try_from(width).ok())
        .ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation digest width overflowed".to_owned(),
            )
        })?;
    if digest_bytes.len() != digest_width {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation digest width changed during encoding".to_owned(),
        ));
    }
    writer
        .seek(SeekFrom::Start(digest_start))
        .map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation digest seek failed: {error}"
            ))
        })?;
    write_chunked(writer, &digest_bytes, maximum_write).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation digest serialization failed: {error}"
        ))
    })?;
    let envelope_end = envelope_start.checked_add(written).ok_or_else(|| {
        CodeIndexProductionErrorV1::Contract(
            "sealed generation writer position overflowed".to_owned(),
        )
    })?;
    writer
        .seek(SeekFrom::Start(envelope_end))
        .map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation final seek failed: {error}"
            ))
        })?;
    Ok(written)
}

fn json_generation_digest(
    generation_bytes: &[u8],
) -> Result<ManifestDigest, CodeIndexProductionErrorV1> {
    ManifestDigest::from_sha256_bytes(&Sha256::digest(generation_bytes))
        .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
}

fn json_generation_bytes_and_digest<T: Serialize>(
    generation: &T,
) -> Result<(Vec<u8>, ManifestDigest), CodeIndexProductionErrorV1> {
    let generation_bytes = serde_json::to_vec(generation).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation serialization failed: {error}"
        ))
    })?;
    let state_digest = json_generation_digest(&generation_bytes)?;
    Ok((generation_bytes, state_digest))
}

impl CodeIndexPublishedGenerationV1 {
    /// Stream the complete sealed generation into one seekable immutable-store
    /// sink. Writes and the total envelope are bounded independently, and the
    /// payload digest is patched in place after the generation has been hashed.
    #[hotpath::measure]
    pub fn write_sealed<W: Write + Seek>(
        &self,
        writer: &mut W,
    ) -> Result<u64, CodeIndexProductionErrorV1> {
        self.validate()?;
        let generation = PersistedPublishedGenerationRefV1 {
            format_revision: SEALED_GENERATION_FORMAT_REVISION_V1,
            manifest: &self.manifest,
            snapshot: &self.snapshot,
            repository_parse_identity: &self.repository_parse_identity,
            ignored_source_admissions: self.ignored_source_roster.admissions(),
            ignored_source_admissions_digest: self.ignored_source_roster.digest(),
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
        let written = write_generation_envelope_with_limits(
            &generation,
            writer,
            MAX_SEALED_CODE_GENERATION_BYTES_V1,
            SEALED_GENERATION_WRITE_CHUNK_BYTES_V1,
        )?;
        crate::hotpath_observe::record_seal_bytes(written);
        Ok(written)
    }

    /// Encode the complete sealed generation in memory for callers that need
    /// an owned wire payload. Durable publication uses [`Self::write_sealed`]
    /// so it never materializes a corpus-sized intermediate buffer.
    pub fn encode_sealed(&self) -> Result<Vec<u8>, CodeIndexProductionErrorV1> {
        let mut sealed = std::io::Cursor::new(Vec::new());
        self.write_sealed(&mut sealed)?;
        Ok(sealed.into_inner())
    }

    /// Restore and revalidate a complete sealed generation.
    #[hotpath::measure]
    pub fn decode_sealed(bytes: &[u8]) -> Result<Self, CodeIndexProductionErrorV1> {
        crate::hotpath_observe::record_seal_bytes(bytes.len() as u64);
        let admitted_len = u64::try_from(bytes.len()).map_err(|_| {
            CodeIndexProductionErrorV1::Contract("sealed generation length exceeds u64".to_owned())
        })?;
        Self::decode_sealed_reader(std::io::Cursor::new(bytes), admitted_len)
    }

    pub fn decode_sealed_reader<R: std::io::Read>(
        reader: R,
        admitted_len: u64,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let bytes = read_admitted_bytes(reader, admitted_len)?;
        let probe: SealedPublishedGenerationFormatProbeV1 = serde_json::from_slice(&bytes)
            .map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation format probe failed: {error}"
                ))
            })?;
        let envelope = match probe.generation.format_revision {
            LEGACY_CANONICAL_SEALED_GENERATION_FORMAT_REVISION => serde_json::from_slice::<
                SealedPublishedGenerationEnvelopeV1,
            >(&bytes)
            .map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation decoding failed: {error}"
                ))
            })?,
            SEALED_GENERATION_FORMAT_REVISION_V1 => {
                let raw: SealedPublishedGenerationRawEnvelopeV1 = serde_json::from_slice(&bytes)
                    .map_err(|error| {
                        CodeIndexProductionErrorV1::Contract(format!(
                            "sealed generation decoding failed: {error}"
                        ))
                    })?;
                let expected_digest = json_generation_digest(raw.generation.get().as_bytes())?;
                if expected_digest != raw.state_digest {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "sealed generation state digest does not match its payload".to_owned(),
                    ));
                }
                let generation: PersistedPublishedGenerationV1 =
                    serde_json::from_str(raw.generation.get()).map_err(|error| {
                        CodeIndexProductionErrorV1::Contract(format!(
                            "sealed generation payload decoding failed: {error}"
                        ))
                    })?;
                if generation.format_revision.0 != SEALED_GENERATION_FORMAT_REVISION_V1 {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "sealed generation format revision is incompatible".to_owned(),
                    ));
                }
                SealedPublishedGenerationEnvelopeV1 {
                    state_digest: raw.state_digest,
                    generation,
                }
            }
            _ => {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "sealed generation format revision is incompatible".to_owned(),
                ));
            }
        };
        let expected_digest = match envelope.generation.format_revision.0 {
            LEGACY_CANONICAL_SEALED_GENERATION_FORMAT_REVISION => sealed_generation_payload_digest(
                LEGACY_CANONICAL_SEALED_GENERATION_FORMAT_REVISION,
                &envelope.generation,
            )?,
            SEALED_GENERATION_FORMAT_REVISION_V1 => envelope.state_digest.clone(),
            _ => {
                return Err(CodeIndexProductionErrorV1::Contract(
                    "sealed generation format revision is incompatible".to_owned(),
                ));
            }
        };
        if expected_digest != envelope.state_digest {
            return Err(CodeIndexProductionErrorV1::Contract(
                "sealed generation state digest does not match its payload".to_owned(),
            ));
        }

        let repository_parse_identity = envelope.generation.repository_parse_identity;
        let ignored_source_roster = IgnoredSourceRosterV1::restore(
            &envelope.generation.snapshot,
            &repository_parse_identity,
            envelope.generation.ignored_source_admissions,
            envelope.generation.ignored_source_admissions_digest,
        )?;

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
            repository_parse_identity,
            ignored_source_roster,
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
            chunk_policy: OnceLock::new(),
            graph_manifest: OnceLock::new(),
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
        Ok(sealed_generation_format_revision_is_compatible(
            probe.generation.format_revision,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct MaximumWriteSink {
        inner: std::io::Cursor<Vec<u8>>,
        maximum_write: usize,
    }

    impl Write for MaximumWriteSink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.maximum_write {
                return Err(std::io::Error::other("write exceeded the fixture bound"));
            }
            self.inner.write(bytes)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.inner.flush()
        }
    }

    impl std::io::Seek for MaximumWriteSink {
        fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(position)
        }
    }

    #[derive(Serialize)]
    struct EnvelopeParityFixture<'a> {
        state_digest: &'a ManifestDigest,
        generation: &'a serde_json::Value,
    }

    #[test]
    fn direct_envelope_encoding_matches_canonical_serde_bytes() {
        let generation = serde_json::json!({
            "format_revision": SEALED_GENERATION_FORMAT_REVISION_V1,
            "manifest": {"generation_id": "generation.parity", "payload": "x".repeat(256)}
        });
        let mut assembled = MaximumWriteSink {
            inner: std::io::Cursor::new(Vec::new()),
            maximum_write: 7,
        };
        write_generation_envelope_with_limits(&generation, &mut assembled, u64::MAX, 7)
            .expect("direct sealed envelope encoding");
        let assembled = assembled.inner.into_inner();
        let generation_bytes =
            serde_json::to_vec(&generation).expect("generation fixture serialization");
        let state_digest =
            json_generation_digest(&generation_bytes).expect("generation fixture digest");
        let prior = serde_json::to_vec(&EnvelopeParityFixture {
            state_digest: &state_digest,
            generation: &generation,
        })
        .expect("serde envelope serialization");

        assert_eq!(assembled, prior);
    }

    #[test]
    fn direct_envelope_encoding_refuses_before_exceeding_its_byte_limit() {
        let generation = serde_json::json!({
            "format_revision": SEALED_GENERATION_FORMAT_REVISION_V1,
            "payload": "x".repeat(256)
        });
        let generation_bytes =
            serde_json::to_vec(&generation).expect("generation fixture serialization");
        let state_digest =
            json_generation_digest(&generation_bytes).expect("generation fixture digest");
        let canonical = serde_json::to_vec(&EnvelopeParityFixture {
            state_digest: &state_digest,
            generation: &generation,
        })
        .expect("canonical envelope serialization");
        let byte_limit = u64::try_from(canonical.len() - 1).expect("fixture length fits u64");
        let mut refused = MaximumWriteSink {
            inner: std::io::Cursor::new(Vec::new()),
            maximum_write: 7,
        };

        let error = write_generation_envelope_with_limits(&generation, &mut refused, byte_limit, 7)
            .expect_err("an oversized envelope must be refused");

        assert!(error.to_string().contains("canonical byte limit"));
        assert!(
            u64::try_from(refused.inner.get_ref().len()).expect("fixture length fits u64")
                <= byte_limit,
            "a refused stream must never write beyond its admitted limit"
        );
    }

    /// Publishing a sealed generation re-encodes its content as one canonical
    /// graph write batch, with record payloads JSON-escaped into string
    /// properties (at most doubling the bytes). A batch bound below that
    /// expansion turns sealed-admissible generations permanently
    /// unpublishable: every activation retry exhausts the graph write budget.
    #[test]
    fn graph_batch_canonical_bound_covers_sealed_admissible_generations() {
        assert!(
            u64::try_from(tracedecay_graph_db::MAX_GRAPH_BATCH_CANONICAL_BYTES)
                .expect("batch canonical bound fits u64")
                >= MAX_SEALED_CODE_GENERATION_BYTES_V1.saturating_mul(2)
        );
    }

    /// Encode and decode share one admission bound, so publication can never
    /// seal a generation that every later load would refuse as corrupt.
    #[test]
    fn sealed_generation_byte_bound_is_symmetric() {
        assert!(admit_sealed_generation_len(MAX_SEALED_CODE_GENERATION_BYTES_V1).is_ok());
        assert!(matches!(
            admit_sealed_generation_len(MAX_SEALED_CODE_GENERATION_BYTES_V1 + 1),
            Err(CodeIndexProductionErrorV1::Contract(message))
                if message.contains("canonical byte limit")
        ));
    }

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
