use std::fmt;
use std::io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::sync::{Arc, OnceLock};

#[cfg(test)]
use serde::de::DeserializeOwned;
use serde::de::{SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

use super::lexical_page_source::scan_layout;
use super::*;

/// The monolithic sealed-generation envelope revision. Every reader that
/// gates on the monolithic format — the publication store, the worker probe,
/// and code-generation retention — must gate on this one value.
pub(super) const MONOLITHIC_SEALED_GENERATION_FORMAT_REVISION: u32 = 6;
/// The partitioned generation manifest revision, which the daemon publishes.
pub const SEALED_GENERATION_FORMAT_REVISION_V1: u32 = 7;

/// The oldest sealed envelope revision this build decodes. Anything below it
/// is refused by [`superseded_sealed_generation_revision`] instead of being
/// migrated — a generation is re-derivable from its source tree, so the
/// daemon rebuilds rather than carrying a decoder per retired shape.
pub const MINIMUM_SEALED_GENERATION_FORMAT_REVISION: u32 =
    MONOLITHIC_SEALED_GENERATION_FORMAT_REVISION;

/// The typed refusal for a sealed generation this build no longer reads.
pub fn superseded_sealed_generation_revision(revision: u32) -> CodeIndexProductionErrorV1 {
    CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(revision)
}

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
        MONOLITHIC_SEALED_GENERATION_FORMAT_REVISION | SEALED_GENERATION_FORMAT_REVISION_V1
    )
}

pub fn sealed_generation_payload_digest<T: Serialize>(
    format_revision: u32,
    generation: &T,
) -> Result<ManifestDigest, CodeIndexProductionErrorV1> {
    match format_revision {
        MONOLITHIC_SEALED_GENERATION_FORMAT_REVISION | SEALED_GENERATION_FORMAT_REVISION_V1 => {
            json_generation_bytes_and_digest(generation).map(|(_, digest)| digest)
        }
        revision if revision < MINIMUM_SEALED_GENERATION_FORMAT_REVISION => {
            Err(superseded_sealed_generation_revision(revision))
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
pub(super) struct PersistedFileGenerationArtifactsRefV1<'a> {
    pub(super) authority: &'a ReceiptBoundCodeFileAuthorityV1,
    pub(super) extraction: &'a ExtractionBatchV1,
    pub(super) artifacts: &'a CodeFileIndexArtifactsV1,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct CompatibleSealedFormatRevisionV1(pub(super) u32);

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
                if revision < MINIMUM_SEALED_GENERATION_FORMAT_REVISION {
                    superseded_sealed_generation_revision(revision).to_string()
                } else {
                    "sealed generation format revision is incompatible".to_owned()
                },
            ));
        }
        Ok(Self(revision))
    }
}

/// The sealed `files` array, decoded page by page. The visitor is pure
/// decode: each persist page accumulates exactly once (the pages are the
/// restored corpus), and the CPU-bound authority reconstruction is deferred
/// to [`assemble_published_generation`]'s pool fan-out so the deserializer
/// thread never serializes corpus-scale digest work.
pub(super) struct StreamingRestoredFilesV1 {
    pub(super) files: Vec<PersistedFileGenerationArtifactsV1>,
}

impl<'de> Deserialize<'de> for StreamingRestoredFilesV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct FilesVisitor;

        impl<'de> Visitor<'de> for FilesVisitor {
            type Value = StreamingRestoredFilesV1;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a sealed generation files array")
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut files = Vec::new();
                if let Some(hint) = seq.size_hint() {
                    files.reserve(hint);
                }
                while let Some(file) = seq.next_element::<PersistedFileGenerationArtifactsV1>()? {
                    files.push(file);
                }
                Ok(StreamingRestoredFilesV1 { files })
            }
        }

        deserializer.deserialize_seq(FilesVisitor)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StreamingPersistedPublishedGenerationV1 {
    pub(super) format_revision: CompatibleSealedFormatRevisionV1,
    pub(super) manifest: CodeGenerationManifestV1,
    pub(super) snapshot: SanitizedCodeSnapshotV1,
    pub(super) repository_parse_identity: CodeIndexRepositoryParseIdentityV1,
    pub(super) ignored_source_admissions: Vec<CodeIndexIgnoredSourceAdmissionV1>,
    pub(super) ignored_source_admissions_digest: ManifestDigest,
    pub(super) files: StreamingRestoredFilesV1,
    pub(super) lineage: Vec<SymbolLineageCandidateV1>,
    pub(super) coverage: CoverageSummaryV1,
    pub(super) capability: CodeIndexCapabilityManifestV1,
    pub(super) projection_request: ProjectionBatchRequestV1,
    pub(super) projection_receipt: ProjectionBatchReceiptV1,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StreamingSealedEnvelopeV1 {
    state_digest: ManifestDigest,
    generation: StreamingPersistedPublishedGenerationV1,
}

/// Rebuild every file's parser-backed exact authority on the indexing pool,
/// then move each persist page into its published artifact.
///
/// The digest remints are by-ref and independent per file, so the fan-out
/// keeps the sequential failure semantics (lowest-index error) while the
/// pages themselves move — the persist corpus is never copied.
pub(super) fn restore_file_pages(
    pages: Vec<PersistedFileGenerationArtifactsV1>,
) -> Result<Vec<Arc<FileGenerationArtifactsV1>>, CodeIndexProductionErrorV1> {
    let authorities = collect_bounded_ordered(&pages, |page, _worker| {
        hotpath::measure_block!(
            "code_index.sealed_decode.file_page",
            ExactExtractionAuthorityV1::restore(&page.artifacts.chunks)
                .map_err(CodeIndexProductionErrorV1::Chunk)
        )
    })?;
    Ok(pages
        .into_iter()
        .zip(authorities)
        .map(|(page, exact_authority)| {
            Arc::new(FileGenerationArtifactsV1 {
                authority: page.authority,
                extraction: page.extraction,
                artifacts: page.artifacts,
                exact_authority,
            })
        })
        .collect())
}

pub(super) fn assemble_published_generation(
    generation: StreamingPersistedPublishedGenerationV1,
) -> Result<CodeIndexPublishedGenerationV1, CodeIndexProductionErrorV1> {
    let StreamingPersistedPublishedGenerationV1 {
        format_revision: _,
        manifest,
        snapshot,
        repository_parse_identity,
        ignored_source_admissions,
        ignored_source_admissions_digest,
        files: StreamingRestoredFilesV1 { files },
        lineage,
        coverage,
        capability,
        projection_request,
        projection_receipt,
    } = generation;
    let files = hotpath::measure_block!(
        "code_index.sealed_decode.page_restore",
        restore_file_pages(files)
    )?;
    let (ignored_source_roster, chunks, symbols, imports, edges, edge_abstentions, projection) =
        hotpath::measure_block!("code_index.sealed_decode.authority_restore", {
            let ignored_source_roster = IgnoredSourceRosterV1::restore(
                &snapshot,
                &repository_parse_identity,
                ignored_source_admissions,
                ignored_source_admissions_digest,
            )?;
            // Persist pages moved into `files` exactly once, and chunk/symbol
            // rows are `Arc`-shared between those pages and the generation
            // aggregates: this flatten clones row pointers and per-file
            // document manifests, never a second owned copy of the corpus.
            let chunk_rows = files
                .iter()
                .map(|file| file.artifacts.chunks.clone())
                .collect::<Vec<_>>();
            let symbol_rows = files
                .iter()
                .flat_map(|file| file.artifacts.symbols.iter().cloned())
                .collect::<Vec<_>>();
            let chunks = GenerationChunkManifestV1::new(manifest.generation_id.clone(), chunk_rows)
                .map_err(CodeIndexProductionErrorV1::Increment)?;
            let symbols = GenerationSymbolIndexV1::new(manifest.generation_id.clone(), symbol_rows)
                .map_err(CodeIndexProductionErrorV1::Lineage)?;
            let imports = derive_import_evidence(&files);
            let (edges, edge_abstentions) = collect_edge_evidence(&files);
            let projection =
                ProjectionPublicationHandoffV1::restore(projection_request, projection_receipt)
                    .map_err(CodeIndexProductionErrorV1::Projection)?;
            Ok::<_, CodeIndexProductionErrorV1>((
                ignored_source_roster,
                chunks,
                symbols,
                imports,
                edges,
                edge_abstentions,
                projection,
            ))
        })?;
    let published = CodeIndexPublishedGenerationV1 {
        manifest,
        snapshot,
        repository_parse_identity,
        ignored_source_roster,
        files,
        chunks,
        symbols,
        lineage,
        imports,
        edges,
        edge_abstentions,
        coverage,
        capability,
        projection,
        validated: OnceLock::new(),
        admitted: OnceLock::new(),
        attribution: OnceLock::new(),
        chunk_policy: OnceLock::new(),
        graph_manifest: OnceLock::new(),
    };
    hotpath::measure_block!(
        "code_index.sealed_decode.corpus_validation",
        published.validate_fresh()
    )?;
    Ok(published)
}

#[derive(Serialize)]
struct PersistedPublishedGenerationRefV1<'a> {
    format_revision: u32,
    manifest: &'a CodeGenerationManifestV1,
    snapshot: &'a SanitizedCodeSnapshotV1,
    repository_parse_identity: &'a CodeIndexRepositoryParseIdentityV1,
    ignored_source_admissions: &'a [CodeIndexIgnoredSourceAdmissionV1],
    ignored_source_admissions_digest: &'a ManifestDigest,
    /// Pre-encoded file JSON. Each file is serialized on the indexing pool
    /// before the envelope is stitched so a 700+ file generation does not
    /// pay a single-threaded `to_writer` of the whole files array.
    files: Vec<Box<RawValue>>,
    lineage: &'a [SymbolLineageCandidateV1],
    coverage: CoverageSummaryV1,
    capability: &'a CodeIndexCapabilityManifestV1,
    projection_request: &'a ProjectionBatchRequestV1,
    projection_receipt: &'a ProjectionBatchReceiptV1,
}

#[derive(Deserialize)]
struct SealedPublishedGenerationRawEnvelopeV1<'a> {
    state_digest: ManifestDigest,
    #[serde(borrow)]
    generation: &'a RawValue,
}

#[derive(Deserialize)]
struct SealedPublishedGenerationFormatProbeV1 {
    generation: PersistedPublishedGenerationFormatProbeV1,
}

#[derive(Deserialize)]
struct PersistedPublishedGenerationFormatProbeV1 {
    format_revision: u32,
}

/// Materialize one sealed monolithic envelope with the fewest corpus-scale
/// passes: `None` means the bytes belong to another decoder (a partitioned
/// manifest), and a superseded revision is refused outright.
///
/// The happy path is exactly one boundary parse (isolating the payload
/// bytes), one payload digest, and one typed materialization. The standalone
/// format probe runs only when that single-pass decode cannot accept the
/// bytes, where [`classify_unaccepted_envelope`] decides whether the
/// interrupting rejection is real or the bytes are simply not monolithic. The
/// digest is computed and compared before the payload is materialized, so a
/// corrupt envelope is still rejected without building corpus-scale
/// structures from unverified bytes.
fn materialize_compatible_envelope(
    bytes: &[u8],
) -> Result<Option<StreamingPersistedPublishedGenerationV1>, CodeIndexProductionErrorV1> {
    let raw: SealedPublishedGenerationRawEnvelopeV1 = match hotpath::measure_block!(
        "code_index.generation.decode.raw_envelope_parse",
        serde_json::from_slice(bytes)
    ) {
        Ok(raw) => raw,
        Err(error) => {
            return classify_unaccepted_envelope(
                bytes,
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation decoding failed: {error}"
                )),
            );
        }
    };
    let payload_digest = hotpath::measure_block!(
        "code_index.sealed_decode.v6_payload_digest",
        json_generation_digest(raw.generation.get().as_bytes())
    )?;
    if payload_digest != raw.state_digest {
        // Corrupt under the monolithic raw-bytes rule, but the probe — never
        // the digest — decides which revision owns these bytes, so the
        // format-revision gate stays authoritative.
        return classify_unaccepted_envelope(
            bytes,
            CodeIndexProductionErrorV1::Contract(
                "sealed generation state digest does not match its payload".to_owned(),
            ),
        );
    }
    let streamed: Result<StreamingPersistedPublishedGenerationV1, _> = hotpath::measure_block!(
        "code_index.sealed_decode.persisted_materialization",
        serde_json::from_str(raw.generation.get())
    );
    match streamed {
        Ok(generation)
            if generation.format_revision.0 == MONOLITHIC_SEALED_GENERATION_FORMAT_REVISION =>
        {
            Ok(Some(generation))
        }
        // The only other admitted revision is the partitioned manifest, which
        // this decoder does not own.
        Ok(_) => Ok(None),
        Err(error) => classify_unaccepted_envelope(
            bytes,
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation payload decoding failed: {error}"
            )),
        ),
    }
}

/// Probe-first classification for bytes the single-pass decode did not
/// accept: a monolithic revision keeps the exact typed rejection that
/// interrupted the decode, a superseded revision is refused so the caller
/// rebuilds from source, and anything else abstains for another decoder.
fn classify_unaccepted_envelope(
    bytes: &[u8],
    monolithic_rejection: CodeIndexProductionErrorV1,
) -> Result<Option<StreamingPersistedPublishedGenerationV1>, CodeIndexProductionErrorV1> {
    let probe: SealedPublishedGenerationFormatProbeV1 = hotpath::measure_block!(
        "code_index.generation.decode.format_probe",
        serde_json::from_slice(bytes).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation format probe failed: {error}"
            ))
        })
    )?;
    match probe.generation.format_revision {
        MONOLITHIC_SEALED_GENERATION_FORMAT_REVISION => Err(monolithic_rejection),
        revision if revision < MINIMUM_SEALED_GENERATION_FORMAT_REVISION => {
            Err(superseded_sealed_generation_revision(revision))
        }
        _ => Ok(None),
    }
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

fn admit_sealed_generation_bytes(
    bytes: &[u8],
    admitted_len: u64,
) -> Result<&[u8], CodeIndexProductionErrorV1> {
    admit_sealed_generation_len(admitted_len)?;
    let actual_len = u64::try_from(bytes.len()).map_err(|_| {
        CodeIndexProductionErrorV1::Contract("sealed generation length exceeds u64".to_owned())
    })?;
    if actual_len != admitted_len {
        return Err(CodeIndexProductionErrorV1::Contract(
            "sealed generation length does not match its admitted length".to_owned(),
        ));
    }
    Ok(bytes)
}

const SEALED_GENERATION_WRITE_CHUNK_BYTES_V1: usize = 1024 * 1024;

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
    let mut writer = BufWriter::with_capacity(maximum_write, writer);
    let (digest_start, digest_end, generation_hash, written) = {
        let mut bounded = BoundedChunkWriterV1 {
            writer: &mut writer,
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
        bounded.flush().map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation serialization flush failed: {error}"
            ))
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
    write_chunked(&mut writer, &digest_bytes, maximum_write).map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation digest serialization failed: {error}"
        ))
    })?;
    writer.flush().map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation digest flush failed: {error}"
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
    writer.flush().map_err(|error| {
        CodeIndexProductionErrorV1::Contract(format!(
            "sealed generation final flush failed: {error}"
        ))
    })?;
    Ok(written)
}

fn encode_persisted_files_parallel(
    files: &[Arc<FileGenerationArtifactsV1>],
) -> Result<Vec<Box<RawValue>>, CodeIndexProductionErrorV1> {
    hotpath::measure_block!("code_index.sealed_encode.files", {
        super::collect_bounded_ordered(files, |file, _| {
            let persisted = PersistedFileGenerationArtifactsRefV1 {
                authority: &file.authority,
                extraction: &file.extraction,
                artifacts: &file.artifacts,
            };
            serde_json::value::to_raw_value(&persisted).map_err(|error| {
                CodeIndexProductionErrorV1::Contract(format!(
                    "sealed generation file serialization failed: {error}"
                ))
            })
        })
    })
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
    #[hotpath::measure(label = "code_index.sealed_encode.write")]
    pub fn write_sealed<W: Write + Seek>(
        &self,
        writer: &mut W,
    ) -> Result<u64, CodeIndexProductionErrorV1> {
        self.validate()?;
        let files = encode_persisted_files_parallel(&self.files)?;
        let generation = PersistedPublishedGenerationRefV1 {
            format_revision: MONOLITHIC_SEALED_GENERATION_FORMAT_REVISION,
            manifest: &self.manifest,
            snapshot: &self.snapshot,
            repository_parse_identity: &self.repository_parse_identity,
            ignored_source_admissions: self.ignored_source_roster.admissions(),
            ignored_source_admissions_digest: self.ignored_source_roster.digest(),
            files,
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
    #[hotpath::measure(label = "code_index.sealed_decode")]
    pub fn decode_sealed(bytes: &[u8]) -> Result<Self, CodeIndexProductionErrorV1> {
        Self::decode_sealed_if_compatible(bytes)?.ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation format revision is incompatible".to_owned(),
            )
        })
    }

    /// Restore one compatible sealed generation without a separate format
    /// probe over the same corpus-sized byte slice.
    #[hotpath::measure(label = "code_index.generation.decode")]
    pub fn decode_sealed_if_compatible(
        bytes: &[u8],
    ) -> Result<Option<Self>, CodeIndexProductionErrorV1> {
        let admitted_len = u64::try_from(bytes.len()).map_err(|_| {
            CodeIndexProductionErrorV1::Contract("sealed generation length exceeds u64".to_owned())
        })?;
        Self::decode_admitted_sealed_bytes_if_compatible(bytes, admitted_len)
    }

    pub fn decode_sealed_reader<R: std::io::Read>(
        reader: R,
        admitted_len: u64,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        let bytes = hotpath::measure_block!(
            "code_index.sealed_decode.admitted_read",
            read_admitted_bytes(reader, admitted_len)
        )?;
        Self::decode_admitted_sealed_bytes(&bytes, admitted_len)
    }

    fn decode_admitted_sealed_bytes(
        bytes: &[u8],
        admitted_len: u64,
    ) -> Result<Self, CodeIndexProductionErrorV1> {
        Self::decode_admitted_sealed_bytes_if_compatible(bytes, admitted_len)?.ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract(
                "sealed generation format revision is incompatible".to_owned(),
            )
        })
    }

    fn decode_admitted_sealed_bytes_if_compatible(
        bytes: &[u8],
        admitted_len: u64,
    ) -> Result<Option<Self>, CodeIndexProductionErrorV1> {
        let bytes = hotpath::measure_block!(
            "code_index.sealed_decode.input_admission",
            admit_sealed_generation_bytes(bytes, admitted_len)
        )?;
        crate::hotpath_observe::record_seal_bytes(admitted_len);
        match materialize_compatible_envelope(bytes)? {
            Some(generation) => assemble_published_generation(generation).map(Some),
            None => Ok(None),
        }
    }

    /// Stream one compatible sealed generation from a seekable reader without
    /// holding the envelope bytes or the persist corpus in memory at once.
    ///
    /// The layout scan proves the envelope digest incrementally (the same
    /// `sha256(generation_json)` rule as [`Self::decode_sealed_if_compatible`])
    /// and optionally the durable file digest. Each `files` element is then
    /// restored and dropped before the next is decoded.
    #[hotpath::measure(label = "code_index.generation.decode.seek")]
    pub fn decode_sealed_seek_reader<R: Read + Seek>(
        mut reader: R,
        admitted_len: u64,
        expected_file_digest: Option<&ManifestDigest>,
        control: &dyn CodeIndexExecutionControlV1,
    ) -> Result<Option<Self>, CodeIndexProductionErrorV1> {
        admit_sealed_generation_len(admitted_len)?;
        crate::hotpath_observe::record_seal_bytes(admitted_len);
        let layout = match scan_layout(&mut reader, admitted_len, expected_file_digest, control) {
            Ok(layout) => layout,
            Err(CodeIndexProductionErrorV1::Contract(message))
                if message.contains("format revision is incompatible") =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        reader.seek(SeekFrom::Start(0)).map_err(|error| {
            CodeIndexProductionErrorV1::Contract(format!(
                "sealed generation decode seek failed: {error}"
            ))
        })?;
        match layout.format_revision {
            MONOLITHIC_SEALED_GENERATION_FORMAT_REVISION => {
                let envelope: StreamingSealedEnvelopeV1 = hotpath::measure_block!(
                    "code_index.sealed_decode.persisted_materialization",
                    serde_json::from_reader(BufReader::with_capacity(64 * 1024, reader))
                )
                .map_err(|error| {
                    CodeIndexProductionErrorV1::Contract(format!(
                        "sealed generation payload decoding failed: {error}"
                    ))
                })?;
                if envelope.state_digest != layout.state_digest {
                    return Err(CodeIndexProductionErrorV1::Contract(
                        "sealed generation state digest does not match its payload".to_owned(),
                    ));
                }
                assemble_published_generation(envelope.generation).map(Some)
            }
            // Every other revision the layout scan admits belongs to the
            // partitioned decoder; a superseded one never reaches here because
            // `scan_layout` refuses it outright.
            _ => Ok(None),
        }
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
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    use super::*;

    #[test]
    fn format_gate_accepts_only_the_monolithic_and_partitioned_revisions() {
        assert_eq!(SEALED_GENERATION_FORMAT_REVISION_V1, 7);
        assert_eq!(MINIMUM_SEALED_GENERATION_FORMAT_REVISION, 6);
        assert!(sealed_generation_format_revision_is_compatible(6));
        assert!(sealed_generation_format_revision_is_compatible(7));
        assert!(!sealed_generation_format_revision_is_compatible(5));
        assert!(!sealed_generation_format_revision_is_compatible(8));
    }

    struct LargestAllocationRecorderV1;

    thread_local! {
        static LARGEST_ALLOCATION_BYTES: Cell<usize> = const { Cell::new(0) };
    }

    unsafe impl GlobalAlloc for LargestAllocationRecorderV1 {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            LARGEST_ALLOCATION_BYTES.with(|largest| largest.set(largest.get().max(layout.size())));
            unsafe { System.alloc(layout) }
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            unsafe { System.dealloc(ptr, layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            LARGEST_ALLOCATION_BYTES.with(|largest| largest.set(largest.get().max(layout.size())));
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            LARGEST_ALLOCATION_BYTES.with(|largest| largest.set(largest.get().max(new_size)));
            unsafe { System.realloc(ptr, layout, new_size) }
        }
    }

    #[global_allocator]
    static TEST_ALLOCATOR: LargestAllocationRecorderV1 = LargestAllocationRecorderV1;

    fn measure_largest_allocation<T>(work: impl FnOnce() -> T) -> (T, usize) {
        LARGEST_ALLOCATION_BYTES.with(|largest| largest.set(0));
        let value = work();
        let largest = LARGEST_ALLOCATION_BYTES.with(Cell::get);
        (value, largest)
    }

    struct MaximumWriteSink {
        inner: std::io::Cursor<Vec<u8>>,
        maximum_write: usize,
        write_calls: usize,
        largest_write: usize,
    }

    impl Write for MaximumWriteSink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.maximum_write {
                return Err(std::io::Error::other("write exceeded the fixture bound"));
            }
            self.write_calls += 1;
            self.largest_write = self.largest_write.max(bytes.len());
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
            write_calls: 0,
            largest_write: 0,
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
    fn direct_envelope_encoding_coalesces_small_serialization_writes() {
        const WRITE_BOUND: usize = 1024 * 1024;
        let generation = serde_json::json!({
            "format_revision": SEALED_GENERATION_FORMAT_REVISION_V1,
            "payload": vec![1_u8; WRITE_BOUND]
        });
        let mut assembled = MaximumWriteSink {
            inner: std::io::Cursor::new(Vec::new()),
            maximum_write: WRITE_BOUND,
            write_calls: 0,
            largest_write: 0,
        };

        write_generation_envelope_with_limits(&generation, &mut assembled, u64::MAX, WRITE_BOUND)
            .expect("direct sealed envelope encoding");

        assert!(
            assembled.write_calls <= 8,
            "a two-megabyte seal must use coalesced writes, observed {}",
            assembled.write_calls
        );
        assert!(assembled.largest_write <= WRITE_BOUND);
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
            write_calls: 0,
            largest_write: 0,
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

    fn sealed_fixture(state_digest: &ManifestDigest, generation: &str) -> Vec<u8> {
        format!(
            "{{\"state_digest\":{},\"generation\":{}}}",
            serde_json::to_string(state_digest).expect("fixture digest serialization"),
            generation
        )
        .into_bytes()
    }

    /// An incompatible revision must stay `Ok(None)` on every classification
    /// path: with a payload digest that matches the V1 raw-bytes rule (the
    /// single-pass decode fails inside materialization) and with one that does
    /// not (the digest gate fails first).
    #[test]
    fn incompatible_revision_stays_none_with_and_without_a_matching_payload_digest() {
        let generation = "{\"format_revision\":9}";
        let matching = json_generation_digest(generation.as_bytes()).expect("fixture digest");
        let mismatched = ManifestDigest::from_sha256_bytes(&[0; 32]).expect("fixture digest");

        for state_digest in [matching, mismatched] {
            let sealed = sealed_fixture(&state_digest, generation);
            assert!(matches!(
                CodeIndexPublishedGenerationV1::decode_sealed_if_compatible(&sealed),
                Ok(None)
            ));
            assert!(matches!(
                CodeIndexPublishedGenerationV1::decode_sealed(&sealed),
                Err(CodeIndexProductionErrorV1::Contract(message))
                    if message.contains("format revision is incompatible")
            ));
        }
    }

    /// A superseded revision is refused with the typed rebuild error on every
    /// classification path — never silently abstained like a revision this
    /// decoder simply does not own, because abstention would hand the bytes to
    /// the partitioned decoder and surface them as corruption.
    #[test]
    fn superseded_revision_is_refused_with_the_rebuild_error() {
        let generation = format!(
            "{{\"format_revision\":{}}}",
            MINIMUM_SEALED_GENERATION_FORMAT_REVISION - 1
        );
        let matching = json_generation_digest(generation.as_bytes()).expect("fixture digest");
        let mismatched = ManifestDigest::from_sha256_bytes(&[0; 32]).expect("fixture digest");

        for state_digest in [matching, mismatched] {
            let sealed = sealed_fixture(&state_digest, &generation);
            for decoded in [
                CodeIndexPublishedGenerationV1::decode_sealed_if_compatible(&sealed).err(),
                CodeIndexPublishedGenerationV1::decode_sealed(&sealed).err(),
            ] {
                let error = decoded.expect("a superseded revision must be refused");
                assert!(
                    matches!(
                        error,
                        CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(revision)
                            if revision == MINIMUM_SEALED_GENERATION_FORMAT_REVISION - 1
                    ),
                    "superseded revision reached the wrong rejection: {error}"
                );
                assert!(
                    error.to_string().contains("will be rebuilt from source"),
                    "superseded rejection must tell the operator it rebuilds: {error}"
                );
            }
        }
    }

    #[test]
    fn undecodable_bytes_are_rejected_as_a_format_probe_failure() {
        let error = CodeIndexPublishedGenerationV1::decode_sealed_if_compatible(b"not sealed json")
            .expect_err("garbage bytes must not decode");

        assert!(
            error.to_string().contains("format probe failed"),
            "garbage bytes reached the wrong rejection: {error}"
        );
    }

    /// A revision-six envelope whose digest verifies but whose payload does
    /// not materialize must keep the payload-decoding rejection, never the
    /// probe or digest one.
    #[test]
    fn v1_payload_that_fails_materialization_keeps_the_payload_rejection() {
        let generation =
            format!("{{\"format_revision\":{MONOLITHIC_SEALED_GENERATION_FORMAT_REVISION}}}");
        let state_digest = json_generation_digest(generation.as_bytes()).expect("fixture digest");
        let sealed = sealed_fixture(&state_digest, &generation);

        let error = CodeIndexPublishedGenerationV1::decode_sealed(&sealed)
            .expect_err("an incomplete revision-six payload must not decode");

        assert!(
            error.to_string().contains("payload decoding failed"),
            "incomplete revision-six payload reached the wrong rejection: {error}"
        );
    }

    #[test]
    fn borrowed_decode_does_not_allocate_a_second_corpus_sized_buffer() {
        const PADDING_BYTES: usize = 8 * 1024 * 1024;
        let wrong_digest = ManifestDigest::from_sha256_bytes(&[0; 32]).expect("fixture digest");
        let mut sealed = format!(
            "{{\"state_digest\":{},\"generation\":{{\"format_revision\":{},\"padding\":\"",
            serde_json::to_string(&wrong_digest).expect("fixture digest serialization"),
            MONOLITHIC_SEALED_GENERATION_FORMAT_REVISION,
        )
        .into_bytes();
        sealed.resize(sealed.len() + PADDING_BYTES, b'x');
        sealed.extend_from_slice(b"\"}}");

        let (result, largest_allocation) =
            measure_largest_allocation(|| CodeIndexPublishedGenerationV1::decode_sealed(&sealed));

        assert!(matches!(
            result,
            Err(CodeIndexProductionErrorV1::Contract(message))
                if message.contains("state digest does not match")
        ));
        assert!(
            largest_allocation < sealed.len() / 2,
            "borrowed decode allocated {largest_allocation} bytes for a {} byte sealed input",
            sealed.len()
        );
    }

    #[test]
    fn raw_v6_payload_borrows_the_callers_admitted_bytes() {
        const PADDING_BYTES: usize = 4 * 1024 * 1024;
        let digest = ManifestDigest::from_sha256_bytes(&[0; 32]).expect("fixture digest");
        let mut sealed = format!(
            "{{\"state_digest\":{},\"generation\":{{\"format_revision\":{},\"padding\":\"",
            serde_json::to_string(&digest).expect("fixture digest serialization"),
            MONOLITHIC_SEALED_GENERATION_FORMAT_REVISION,
        )
        .into_bytes();
        sealed.resize(sealed.len() + PADDING_BYTES, b'x');
        sealed.extend_from_slice(b"\"}}");

        let raw: SealedPublishedGenerationRawEnvelopeV1 =
            serde_json::from_slice(&sealed).expect("raw envelope parses");
        let payload_start = raw.generation.get().as_ptr() as usize;
        let admitted_start = sealed.as_ptr() as usize;
        let admitted_end = admitted_start + sealed.len();

        assert!(
            (admitted_start..admitted_end).contains(&payload_start),
            "the raw payload must point into the caller's admitted byte slice"
        );
    }
}
