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
    CodeIndexAtomicPublicationPort, CodeIndexGenerationScopeV1, CodeIndexPreparationPort,
    CodeIndexProductionErrorV1, CodeIndexPublishedBuildV1, CodeIndexPublishedGenerationV1,
    MemorySealedPublicationStoreV1, SealedGenerationFileWindowsV1, SealedGenerationPackLayoutV1,
    SealedGenerationPackReadPassV1, SealedGenerationSegmentIdentityV1,
    SealedGenerationSegmentPublicationV1, SealedGenerationSegmentReadV1,
    SharedDecodedContentPoolV1, VerifiedSealedLexicalPageSourceV1,
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
use tracedecay_runtime_core::resident_memory::{
    ProcessResidentMemoryV1, ResidentMemoryOperationV1, detected_process_resident_memory_limit_v1,
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
            omitted_sources: Vec::new(),
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

/// Decode the published generation explicitly through its canonical sealed reader.
pub fn cold_generation(
    published: &CodeIndexPublishedBuildV1,
) -> Arc<CodeIndexPublishedGenerationV1> {
    let reader = published
        .sealed_generation()
        .begin_read_pass()
        .expect("begin published generation read");
    Arc::new(
        CodeIndexPublishedGenerationV1::decode_partitioned_sealed(
            published.manifest_bytes(),
            &SharedDecodedContentPoolV1::default(),
            |request, buffer| reader(request, buffer),
            // This helper exercises canonical codec restoration, not admission.
            &mut |_| Ok(()),
        )
        .expect("published generation decodes"),
    )
}

pub fn memory_operation() -> ResidentMemoryOperationV1 {
    Arc::new(ProcessResidentMemoryV1::new(
        detected_process_resident_memory_limit_v1(),
    ))
    .begin_operation()
}

/// A partitioned sealed generation: its manifest and canonical physical packs.
#[derive(Debug, PartialEq, Eq)]
pub struct PartitionedSealV1 {
    pub manifest: Vec<u8>,
    pub segments: BTreeMap<String, Vec<u8>>,
}

impl PartitionedSealV1 {
    pub fn of(generation: &CodeIndexPublishedGenerationV1) -> Self {
        let mut store = MemorySealedPublicationStoreV1::default();
        let mut preparation = store
            .prepare(&generation.sealed_scope(), None, &memory_operation())
            .expect("prepare canonical memory seal");
        let manifest = generation
            .encode_partitioned_sealed(&mut preparation)
            .expect("generation seals partitioned");
        let sealed = Self::from_preparation(manifest, &preparation);
        preparation.abort().expect("release fixture staging");
        sealed
    }

    pub fn from_preparation(
        manifest: Vec<u8>,
        preparation: &impl CodeIndexPreparationPort,
    ) -> Self {
        let reader = preparation.pack_reader();
        let segments = CodeIndexPublishedGenerationV1::partitioned_pack_identities(&manifest)
            .expect("canonical physical pack roster")
            .into_iter()
            .map(|identity| {
                let mut bytes = Vec::new();
                reader(
                    SealedGenerationSegmentReadV1::Whole {
                        digest: &identity.digest,
                        size_bytes: identity.size_bytes,
                    },
                    &mut bytes,
                )
                .expect("read finalized fixture pack");
                (identity.digest.as_str().to_owned(), bytes)
            })
            .collect();
        Self { manifest, segments }
    }

    /// Restore `manifest`, which may be a tampered reseal of this seal's own
    /// manifest, against this seal's segments.
    pub fn restore(
        &self,
        manifest: &[u8],
    ) -> Result<CodeIndexPublishedGenerationV1, CodeIndexProductionErrorV1> {
        CodeIndexPublishedGenerationV1::decode_partitioned_sealed(
            manifest,
            &SharedDecodedContentPoolV1::default(),
            |request, buffer| self.read_pack(request, buffer),
            // Tamper and round-trip fixtures exercise the codec independently.
            &mut |_| Ok(()),
        )
    }

    pub fn read_pack(
        &self,
        request: SealedGenerationSegmentReadV1<'_>,
        buffer: &mut Vec<u8>,
    ) -> Result<(), CodeIndexProductionErrorV1> {
        read_pack_bytes(&self.segments, request, buffer)
    }

    pub fn logical_segment_bytes(&self, digest: &ManifestDigest, size_bytes: u64) -> Vec<u8> {
        let layout = SealedGenerationPackLayoutV1::open(&self.manifest).expect("fixture layout");
        let mut bytes = Vec::new();
        layout
            .read(
                SealedGenerationSegmentReadV1::Whole { digest, size_bytes },
                &mut bytes,
                |request, buffer| self.read_pack(request, buffer),
            )
            .expect("fixture logical segment");
        bytes
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
            &mut |request, buffer| {
                source.read_segment(request, buffer, |physical, buffer| {
                    self.read_pack(physical, buffer)
                })
            },
            revision,
            spill,
            &mut |_| Ok(()),
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

    /// Every sealed byte: the manifest plus each physical pack.
    pub fn byte_len(&self) -> usize {
        self.segments.values().map(Vec::len).sum::<usize>() + self.manifest.len()
    }

    pub fn lexical_source(
        &self,
        maximum_page_chunks: usize,
        maximum_page_bytes: usize,
        operation: &ResidentMemoryOperationV1,
    ) -> Result<VerifiedSealedLexicalPageSourceV1, CodeIndexProductionErrorV1> {
        let segments = Arc::new(self.segments.clone());
        VerifiedSealedLexicalPageSourceV1::open_partitioned_sealed(
            &self.manifest,
            self.state_digest(),
            move |_control, _layout| {
                let segments = Arc::clone(&segments);
                let reader: Box<SealedGenerationPackReadPassV1> =
                    Box::new(move |request, buffer, _control| {
                        read_pack_bytes(&segments, request, buffer)
                    });
                Ok(reader)
            },
            maximum_page_chunks,
            maximum_page_bytes,
            operation,
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
        let old_digest = id::<ManifestDigest>(&digest);
        let old_size = descriptor["segment_size_bytes"]
            .as_u64()
            .expect("file segment size");
        let mut segment = inflate_segment(&self.logical_segment_bytes(&old_digest, old_size));
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
        let layout =
            SealedGenerationPackLayoutV1::open(&self.manifest).expect("original pack layout");
        let mut logical = layout
            .content_pack()
            .extents
            .iter()
            .map(|extent| {
                let mut bytes = Vec::new();
                layout
                    .read(
                        SealedGenerationSegmentReadV1::Whole {
                            digest: &extent.digest,
                            size_bytes: extent.size_bytes,
                        },
                        &mut bytes,
                        |request, buffer| self.read_pack(request, buffer),
                    )
                    .expect("original logical extent");
                (extent.digest.clone(), bytes)
            })
            .collect::<BTreeMap<_, _>>();
        let original_still_referenced = envelope["generation"]["file_segments"]
            .as_array()
            .expect("file descriptors")
            .iter()
            .any(|file| file["segment_digest"].as_str() == Some(digest.as_str()));
        if !original_still_referenced {
            logical
                .remove(&old_digest)
                .expect("original logical segment");
        }
        logical.insert(tampered, bytes);
        let metadata = CodeIndexPublishedGenerationV1::partitioned_text_metadata(&self.manifest)
            .expect("original metadata");
        let mut store = MemorySealedPublicationStoreV1::default();
        let mut preparation = store
            .prepare(
                &CodeIndexGenerationScopeV1::for_snapshot(metadata.snapshot()),
                None,
                &memory_operation(),
            )
            .expect("prepare altered content pack");
        let evidence = layout.evidence_pack();
        let evidence_bytes = &self.segments[evidence.digest.as_str()];
        let pages = envelope["generation"]["generation_evidence"]["pages"]
            .as_array()
            .expect("authenticated evidence pages");
        let mut offset = 0_usize;
        for page in pages {
            let page_ordinal = u32::try_from(page["page_ordinal"].as_u64().expect("page ordinal"))
                .expect("bounded page ordinal");
            let page_digest =
                id::<ManifestDigest>(page["page_digest"].as_str().expect("page digest"));
            let length = usize::try_from(page["page_size_bytes"].as_u64().expect("page size"))
                .expect("bounded page size");
            let end = offset.checked_add(length).expect("evidence extent");
            preparation
                .write_segment(
                    SealedGenerationSegmentPublicationV1::GenerationEvidencePage {
                        page_ordinal,
                        page_digest: &page_digest,
                        bytes: &evidence_bytes[offset..end],
                    },
                )
                .expect("stage original evidence page");
            offset = end;
        }
        assert_eq!(offset, evidence_bytes.len());
        preparation
            .write_segment(
                SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit {
                    segment_digest: &evidence.digest,
                    segment_size_bytes: evidence.size_bytes,
                    page_count: u32::try_from(pages.len()).expect("bounded page count"),
                },
            )
            .expect("commit original evidence");
        let roster = logical
            .iter()
            .map(|(digest, bytes)| {
                preparation
                    .write_segment(SealedGenerationSegmentPublicationV1::File { digest, bytes })
                    .expect("stage altered logical bytes");
                SealedGenerationSegmentIdentityV1 {
                    digest: digest.clone(),
                    size_bytes: u64::try_from(bytes.len()).expect("segment size"),
                }
            })
            .collect::<Vec<_>>();
        let pack = preparation
            .finalize_content_pack(&roster)
            .expect("finalize altered content pack");
        envelope["generation"]["content_pack"] =
            serde_json::to_value(pack).expect("altered pack descriptor");
        let sealed = Self::from_preparation(reseal_manifest(envelope), &preparation);
        preparation.abort().expect("release altered staging");
        sealed
    }

    /// The decoded JSON `file` payload of the segment at `file_index`.
    pub fn file_segment_payload(&self, file_index: usize) -> Value {
        let envelope = self.envelope();
        let digest = envelope["generation"]["file_segments"][file_index]["segment_digest"]
            .as_str()
            .expect("file segment digest");
        let size_bytes = envelope["generation"]["file_segments"][file_index]["segment_size_bytes"]
            .as_u64()
            .expect("file segment size");
        inflate_segment(&self.logical_segment_bytes(&id::<ManifestDigest>(digest), size_bytes))["file"].take()
    }
}

pub fn read_pack_bytes(
    packs: &BTreeMap<String, Vec<u8>>,
    request: SealedGenerationSegmentReadV1<'_>,
    buffer: &mut Vec<u8>,
) -> Result<(), CodeIndexProductionErrorV1> {
    buffer.clear();
    let (digest, size_bytes, offset, length) = match request {
        SealedGenerationSegmentReadV1::Whole { digest, size_bytes } => {
            (digest, size_bytes, 0, size_bytes)
        }
        SealedGenerationSegmentReadV1::Range {
            digest,
            size_bytes,
            offset,
            length,
        } => (digest, size_bytes, offset, length),
    };
    let refused = || {
        CodeIndexProductionErrorV1::Contract(
            "fixture physical pack is absent or its extent is invalid".to_owned(),
        )
    };
    let bytes = packs.get(digest.as_str()).ok_or_else(refused)?;
    if u64::try_from(bytes.len()).ok() != Some(size_bytes) {
        return Err(refused());
    }
    let start = usize::try_from(offset).map_err(|_| refused())?;
    let end = usize::try_from(length)
        .ok()
        .and_then(|length| start.checked_add(length))
        .ok_or_else(refused)?;
    buffer.extend_from_slice(bytes.get(start..end).ok_or_else(refused)?);
    Ok(())
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
