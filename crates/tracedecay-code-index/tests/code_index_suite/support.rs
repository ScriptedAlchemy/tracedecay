use std::collections::BTreeMap;
use std::io::{Read as _, Write as _};
use std::sync::Arc;

use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;

use serde_json::Value;
use sha2::{Digest, Sha256};
use tracedecay_code_index::chunks::content_digest;
use tracedecay_code_index::graph_projection::{
    SealedCodeGraphRowsError, build_sealed_code_graph_rows,
};
use tracedecay_code_index::intake::{CodeIndexIntake, ReceiptBoundCodeFileV1, SanitizedCodeIntake};
use tracedecay_code_index::languages::{LanguageRegistry, StaticLanguageRegistry};
use tracedecay_code_index::production::{
    CodeIndexProductionErrorV1, CodeIndexPublishedGenerationV1, SealedGenerationFileWindowsV1,
    SealedGenerationSegmentPublicationV1, SealedGenerationSegmentReadV1,
    VerifiedSealedLexicalPageSourceV1,
};
use tracedecay_domain::{
    CodeGenerationId, FileOccurrenceId, LanguageDescriptorV1, LanguageId, ManifestDigest,
    ProjectId, RepositoryId, SanitizationReceiptId, SanitizedCodeFileV1, SanitizedCodeSnapshotV1,
    SanitizerRevision, SnapshotFileDispositionV1, UtcMicros, ValidatedCodeFileV1,
};
use tracedecay_graph_db::{
    GraphDbError, GraphGenerationManifest, GraphGenerationRowSpill, GraphProjectionIdentity,
    GraphProjectorRevision,
};

pub const RUST_SOURCE: &str = "//! Module documentation.\n\nuse std::collections::HashMap;\n\n/// Increment a value.\npub fn alpha(value: u32) -> u32 {\n    value + 1\n}\n\npub struct Holder {\n    map: HashMap<u32, u32>,\n}\n\nimpl Holder {\n    pub fn get(&self, key: u32) -> Option<u32> {\n        self.map.get(&key).copied()\n    }\n}\n\n// trailing window text\n";

pub use tracedecay_domain::test_fixtures::id;

pub use tracedecay_domain::test_fixtures::digest;

pub fn registry() -> StaticLanguageRegistry {
    StaticLanguageRegistry::new()
}

pub fn rust_descriptor() -> LanguageDescriptorV1 {
    registry()
        .descriptor(&id::<LanguageId>("rust"))
        .expect("rust descriptor")
        .clone()
}

pub fn validated_rust_file(source: &[u8]) -> ReceiptBoundCodeFileV1 {
    let file = SanitizedCodeFileV1 {
        file_occurrence_id: id::<FileOccurrenceId>("file.fixture"),
        logical_path: "src/lib.rs".to_owned(),
        language: Some(id::<LanguageId>("rust")),
        content_digest: content_digest(source),
        disposition: SnapshotFileDispositionV1::Present,
    };
    let intake = SanitizedCodeIntake::new(
        registry(),
        id::<SanitizerRevision>("sanitizer.v1"),
        UtcMicros(1_000_000),
    );
    let capability = intake
        .admit(SanitizedCodeSnapshotV1 {
            repository: id::<RepositoryId>("repo.fixture"),
            worktree: None,
            reference: None,
            source_revision: None,
            sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
            sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.fixture")],
            content_identity: content_digest(source),
            captured_at: UtcMicros(1_000_000),
            files: vec![file.clone()],
        })
        .expect("snapshot capability");
    intake
        .bind_file(
            &capability,
            &id::<ProjectId>("project.fixture"),
            ValidatedCodeFileV1 {
                generation_id: id::<CodeGenerationId>("generation.fixture"),
                file,
                snapshot_digest: capability.snapshot().intake_digest.clone(),
                sanitized_bytes: source.to_vec(),
            },
        )
        .expect("receipt-bound rust source")
}

/// A partitioned sealed generation held in memory: the manifest plus every
/// published segment under its digest, evidence pages assembled into their
/// pack under the commit digest.
#[derive(Debug, PartialEq, Eq)]
pub struct PartitionedSealV1 {
    pub manifest: Vec<u8>,
    pub segments: BTreeMap<String, Vec<u8>>,
}

impl PartitionedSealV1 {
    pub fn of(generation: &CodeIndexPublishedGenerationV1) -> Self {
        let mut segments = BTreeMap::new();
        let mut evidence_pack = Vec::new();
        let manifest = generation
            .encode_partitioned_sealed(|publication| {
                match publication {
                    SealedGenerationSegmentPublicationV1::File { digest, bytes } => {
                        segments.insert(digest.as_str().to_owned(), bytes.to_vec());
                    }
                    SealedGenerationSegmentPublicationV1::GenerationEvidencePage {
                        bytes, ..
                    } => evidence_pack.extend_from_slice(bytes),
                    SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit {
                        segment_digest,
                        ..
                    } => {
                        segments.insert(
                            segment_digest.as_str().to_owned(),
                            std::mem::take(&mut evidence_pack),
                        );
                    }
                }
                Ok(())
            })
            .expect("generation seals partitioned");
        Self { manifest, segments }
    }

    /// Restore `manifest`, which may be a tampered reseal of this seal's own
    /// manifest, against this seal's segments.
    pub fn restore(
        &self,
        manifest: &[u8],
    ) -> Result<CodeIndexPublishedGenerationV1, CodeIndexProductionErrorV1> {
        CodeIndexPublishedGenerationV1::decode_partitioned_sealed(manifest, |request, buffer| {
            self.read_segment(request, buffer)
        })
    }

    fn read_segment(
        &self,
        request: SealedGenerationSegmentReadV1<'_>,
        buffer: &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        let (digest, offset, length) = match request {
            SealedGenerationSegmentReadV1::Whole { digest, size_bytes } => (digest, 0, size_bytes),
            SealedGenerationSegmentReadV1::Range {
                digest,
                offset,
                length,
                ..
            } => (digest, offset, length),
        };
        let bytes = self.segments.get(digest.as_str()).ok_or_else(|| {
            CodeIndexProductionErrorV1::Contract("fixture segment is missing".to_owned())
        })?;
        let start = usize::try_from(offset).expect("segment offset fits usize");
        let end = start + usize::try_from(length).expect("segment length fits usize");
        buffer.clear();
        buffer.extend_from_slice(&bytes[start..end]);
        Ok(())
    }

    /// The code graph this seal projects, built the way publication builds
    /// it, from the segments one window of files at a time, then read back
    /// as one manifest.
    pub fn graph_manifest(
        &self,
        projection: GraphProjectionIdentity,
        revision: &GraphProjectorRevision,
    ) -> GraphGenerationManifest {
        self.graph_manifest_checked(projection, revision, &|| Ok(()))
            .expect("sealed generation projects")
    }

    pub fn graph_manifest_checked(
        &self,
        projection: GraphProjectionIdentity,
        revision: &GraphProjectorRevision,
        check: &dyn Fn() -> Result<(), GraphDbError>,
    ) -> Result<GraphGenerationManifest, SealedCodeGraphRowsError> {
        let scratch = tempfile::tempdir().expect("graph row scratch directory");
        let spill =
            GraphGenerationRowSpill::create(scratch.path().join("rows"), projection.clone())?;
        let source = SealedGenerationFileWindowsV1::open(&self.manifest)?;
        let spilled = build_sealed_code_graph_rows(
            projection,
            &source,
            &mut |request, buffer| self.read_segment(request, buffer),
            revision,
            spill,
            check,
        )?;
        Ok(spilled.materialize(&|| Ok(()))?)
    }

    pub fn restored(&self) -> CodeIndexPublishedGenerationV1 {
        self.restore(&self.manifest)
            .expect("partitioned generation restores")
    }

    pub fn envelope(&self) -> Value {
        serde_json::from_slice(&self.manifest).expect("partitioned manifest envelope")
    }

    /// The manifest's content address, the lexical source state digest.
    pub fn state_digest(&self) -> ManifestDigest {
        ManifestDigest::from_sha256_bytes(&Sha256::digest(&self.manifest))
            .expect("manifest digest is canonical")
    }

    /// Every sealed byte: the manifest plus each segment.
    pub fn byte_len(&self) -> usize {
        self.segments.values().map(Vec::len).sum::<usize>() + self.manifest.len()
    }

    pub fn lexical_source(
        &self,
        maximum_page_chunks: usize,
        maximum_page_bytes: usize,
    ) -> Result<VerifiedSealedLexicalPageSourceV1, CodeIndexProductionErrorV1> {
        let segments = Arc::new(self.segments.clone());
        VerifiedSealedLexicalPageSourceV1::open_partitioned_sealed(
            &self.manifest,
            self.state_digest(),
            move |digest, _, buffer, _control| {
                let bytes = segments.get(digest.as_str()).ok_or_else(|| {
                    CodeIndexProductionErrorV1::Contract("fixture segment is missing".to_owned())
                })?;
                buffer.clear();
                buffer.extend_from_slice(bytes);
                Ok(())
            },
            maximum_page_chunks,
            maximum_page_bytes,
        )
    }

    /// A copy of this seal whose file segment at `file_index` carries
    /// `mutate` applied to its `file` payload, re-addressed in the manifest
    /// and resealed, so a refusal exercises the file payload rather than a
    /// content-address or state-digest check.
    pub fn with_tampered_file_segment(
        &self,
        file_index: usize,
        mutate: impl FnOnce(&mut Value),
    ) -> Self {
        let mut envelope = self.envelope();
        let descriptor = &mut envelope["generation"]["file_segments"][file_index];
        let digest = descriptor["segment_digest"]
            .as_str()
            .expect("file segment digest")
            .to_owned();
        let mut segment = inflate_segment(&self.segments[&digest]);
        mutate(&mut segment["file"]);
        let canonical = serde_json::to_vec(&segment).expect("tampered segment serializes");
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(&canonical)
            .expect("tampered segment compresses");
        let bytes = encoder.finish().expect("tampered segment compresses");
        let tampered = ManifestDigest::from_sha256_bytes(&Sha256::digest(&bytes))
            .expect("tampered segment digest");
        descriptor["segment_digest"] = Value::String(tampered.as_str().to_owned());
        descriptor["segment_size_bytes"] = Value::from(bytes.len() as u64);
        descriptor["decoded_size_bytes"] = Value::from(canonical.len() as u64);
        let mut segments = self.segments.clone();
        segments.insert(tampered.as_str().to_owned(), bytes);
        Self {
            manifest: reseal_manifest(envelope),
            segments,
        }
    }

    /// The decoded JSON `file` payload of the segment at `file_index`.
    pub fn file_segment_payload(&self, file_index: usize) -> Value {
        let envelope = self.envelope();
        let digest = envelope["generation"]["file_segments"][file_index]["segment_digest"]
            .as_str()
            .expect("file segment digest");
        inflate_segment(&self.segments[digest])["file"].take()
    }
}

fn inflate_segment(bytes: &[u8]) -> Value {
    let mut canonical = Vec::new();
    DeflateDecoder::new(bytes)
        .read_to_end(&mut canonical)
        .expect("file segment inflates");
    serde_json::from_slice(&canonical).expect("file segment JSON")
}

/// Re-authenticate a tampered partitioned manifest envelope so its refusal
/// exercises the payload rather than the outer state digest.
pub fn reseal_manifest(mut envelope: Value) -> Vec<u8> {
    let generation =
        serde_json::to_vec(&envelope["generation"]).expect("manifest generation serializes");
    let state_digest = ManifestDigest::from_sha256_bytes(&Sha256::digest(generation))
        .expect("manifest digest is canonical");
    envelope["state_digest"] = Value::String(state_digest.as_str().to_owned());
    serde_json::to_vec(&envelope).expect("resealed manifest serializes")
}
