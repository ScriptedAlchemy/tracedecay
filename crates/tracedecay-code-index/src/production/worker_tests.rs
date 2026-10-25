use std::sync::atomic::{AtomicUsize, Ordering};
use tracedecay_runtime_core::resident_memory::{
    ProcessResidentMemoryV1, ProcessResidentSampleV1, ResidentMemoryPressureV1,
    detected_process_resident_memory_limit_v1,
};

use sha2::Digest as _;

use tracedecay_domain::{
    ChunkerRevision, ExtractorRevision, LanguageId, PrivacyDomainId, ProjectionKeyV1,
    ProjectionKindV1, SanitizationReceiptId, SanitizerRevision,
};

use super::*;

pub(super) type WorkerPublicationStore = MemorySealedPublicationStoreV1;

pub(super) fn decoded_generation(
    published: &CodeIndexPublishedBuildV1,
) -> Arc<CodeIndexPublishedGenerationV1> {
    let reader = published.sealed_generation().segment_reader();
    Arc::new(
        CodeIndexPublishedGenerationV1::decode_partitioned_sealed(
            published.manifest_bytes(),
            &SharedDecodedContentPoolV1::default(),
            |request, buffer| reader(request, buffer),
        )
        .expect("published generation decodes"),
    )
}

pub(super) fn worker_id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid fixture identity")
}

pub(super) fn worker_config() -> CodeIndexProductionConfigV1 {
    CodeIndexProductionConfigV1 {
        project_id: worker_id("project.worker"),
        repository: worker_id("repository.worker"),
        sanitizer_revision: worker_id::<SanitizerRevision>("sanitizer.v1"),
        policy_revision: worker_id::<PolicyRevisionId>("policy.v1"),
        chunker_revision: worker_id::<ChunkerRevision>("chunker.v2"),
        privacy_domain: worker_id::<PrivacyDomainId>("privacy.worker"),
        privacy_key_epoch: 1,
        max_snapshot_age_micros: None,
    }
}

pub(super) fn worker_request_with_source(
    file_occurrence: &str,
    sealed_at: i64,
    source: &[u8],
) -> CodeIndexBuildRequestV1 {
    let file = SanitizedCodeFileV1 {
        file_occurrence_id: worker_id(file_occurrence),
        logical_path: "src/lib.rs".to_owned(),
        language: Some(worker_id::<LanguageId>("rust")),
        content_digest: content_digest(source),
        disposition: SnapshotFileDispositionV1::Present,
    };
    CodeIndexBuildRequestV1 {
        resident_memory: std::sync::Arc::new(ProcessResidentMemoryV1::new(
            detected_process_resident_memory_limit_v1(),
        )),
        snapshot: SanitizedCodeSnapshotV1 {
            repository: worker_id("repository.worker"),
            worktree: None,
            reference: None,
            source_revision: None,
            sanitizer_revision: worker_id("sanitizer.v1"),
            sanitization_receipts: vec![worker_id::<SanitizationReceiptId>("receipt.worker")],
            content_identity: content_digest(source),
            captured_at: UtcMicros(1_000_000),
            files: vec![file.clone()],
            omitted_sources: Vec::new(),
        },
        captured_files: vec![CodeIndexCapturedFileV1 {
            file_occurrence_id: file.file_occurrence_id,
            sanitized_bytes: Arc::from(source),
            sensitivity_level: SensitivityLevelV1::Public,
        }],
        changed_files: BTreeSet::new(),
        invalidations: BTreeSet::new(),
        ignored_source_admissions: Vec::new(),
        repository_parse_identity: CodeIndexRepositoryParseIdentityV1 {
            tree: None,
            dirty: tracedecay_domain::RepositoryDirtyStateV1::Dirty,
        },
        target_projection_key: ProjectionKeyV1 {
            kind: ProjectionKindV1::Lexical,
            schema_revision: "lexical.v1".to_owned(),
            profile_digest: worker_id(&format!("sha256:{}", "a".repeat(64))),
        },
        sealed_at: UtcMicros(sealed_at),
    }
}

fn worker_request(file_occurrence: &str, sealed_at: i64) -> CodeIndexBuildRequestV1 {
    worker_request_with_source(file_occurrence, sealed_at, b"")
}

#[derive(Debug, PartialEq, Eq)]
enum WorkerTestError {
    Mapping(usize),
    Parallelism(crate::parallelism::CodeIndexParallelismErrorV1),
}

impl From<crate::parallelism::CodeIndexParallelismErrorV1> for WorkerTestError {
    fn from(error: crate::parallelism::CodeIndexParallelismErrorV1) -> Self {
        Self::Parallelism(error)
    }
}

fn construction_ledger(limit: u64) -> Arc<ProcessResidentMemoryV1> {
    let limit = NonZeroU64::new(limit).expect("nonzero test budget");
    let pressure = Arc::new(ResidentMemoryPressureV1::with_sampler(
        limit,
        Arc::new(|| {
            Some(ProcessResidentSampleV1 {
                resident_bytes: 0,
                unreclaimable_bytes: 0,
                swapped_bytes: 0,
                cgroup_headroom_bytes: None,
            })
        }),
    ));
    Arc::new(ProcessResidentMemoryV1::with_pressure(limit, pressure))
}

#[test]
fn canonical_construction_admission_precedes_hashing_and_releases_its_guards() {
    let mut request =
        worker_request_with_source("file.worker.hash-admission", 1_100_000, b"fn target() {}\n");
    request.resident_memory = construction_ledger(64 * 1024 * 1024);
    let workspace = metadata_hash_workspace(
        &{
            let mut owner =
                CodeIndexProductionOwnerV1::new(worker_config(), WorkerPublicationStore::default())
                    .unwrap();
            owner
                .build_and_publish(request.clone(), &UninterruptibleCodeIndexControlV1)
                .unwrap()
                .manifest()
                .clone()
        },
        &request.snapshot,
    )
    .unwrap();
    let ledger = construction_ledger(1024);
    let operation = ledger.begin_operation();
    let mut memory = ConstructionMemoryV1::new(operation.clone()).unwrap();
    let entered = std::cell::Cell::new(false);
    let denied: Result<ManifestDigest, _> = memory.canonical(0, workspace, || {
        entered.set(true);
        canonical_sha256(&request.snapshot)
            .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
    });
    assert!(matches!(
        denied,
        Err(CodeIndexProductionErrorV1::Publication(
            CodeIndexPublicationStoreErrorV1::ResidentMemoryRefused(_)
        ))
    ));
    assert!(
        !entered.get(),
        "a refused preflight must never enter canonical serialization"
    );
    assert!(
        operation
            .witnessed_reconstruction_requirement(ledger.limit_bytes())
            .unwrap()
            .is_some()
    );
    drop(memory);
    assert_eq!(ledger.snapshot().used_bytes, 0);

    let ledger = construction_ledger(16 * 1024 * 1024);
    let mut memory = ConstructionMemoryV1::new(ledger.begin_operation()).unwrap();
    let expected = canonical_sha256(&request.snapshot).unwrap();
    let actual = memory
        .canonical(0, workspace, || {
            canonical_sha256(&request.snapshot)
                .map_err(|error| CodeIndexProductionErrorV1::Contract(error.to_string()))
        })
        .unwrap();
    assert_eq!(
        actual, expected,
        "admission must preserve the canonical hash bytes"
    );
    assert!(ledger.snapshot().used_bytes >= memory.charged_bytes);
    drop(memory);
    assert_eq!(ledger.snapshot().used_bytes, 0);
}

#[test]
fn cold_finalization_denies_before_emitting_and_preserves_sealed_wire_bytes() {
    let mut owner =
        CodeIndexProductionOwnerV1::new(worker_config(), WorkerPublicationStore::default())
            .unwrap();
    let mut request = worker_request_with_source(
        "file.worker.finalization-admission",
        1_100_000,
        b"pub fn target() {}\npub fn caller() { target(); }\n",
    );
    request.resident_memory = construction_ledger(64 * 1024 * 1024);
    let built = owner
        .build_and_publish(request, &UninterruptibleCodeIndexControlV1)
        .unwrap();
    let decoded = decoded_generation(&built);
    assert_eq!(
        partitioned_seal(&decoded).0,
        built.manifest_bytes().as_ref(),
        "cold charged finalization must match the complete-input encoder byte for byte"
    );
    let held = built.publication().construction_memory.as_ref().unwrap();
    let retained = resident_bytes::projection_heap_bytes(built.publication().projection())
        .saturating_add(built.manifest_bytes().len())
        .saturating_add(resident_bytes::compact_metadata_bytes(
            &built.publication().metadata,
            resident_bytes::chunk_policy_heap_bytes(decoded.chunk_policy_summary()),
        ));
    assert!(
        held.charged_bytes as usize >= retained,
        "compact metadata must remain charged after preparation"
    );

    let changes = &decoded.projection.request().changes.added_or_changed;
    let keys = vec![0_u32; changes.len()];
    let denied_ledger = construction_ledger(1);
    let mut denied_memory = ConstructionMemoryV1::new(denied_ledger.begin_operation()).unwrap();
    assert!(
        projection_rows::FileChunkRostersV1::from_changes(changes, &keys, 0, &mut |bytes| {
            denied_memory.resize(bytes)
        })
        .is_err()
    );
    let ledger = construction_ledger(16 * 1024 * 1024);
    let mut memory = ConstructionMemoryV1::new(ledger.begin_operation()).unwrap();
    let rosters =
        projection_rows::FileChunkRostersV1::from_changes(changes, &keys, 0, &mut |bytes| {
            memory.resize(bytes)
        })
        .unwrap();
    assert!(
        partitioned_codec::PartitionedGenerationEvidenceRefV1::new_charged(
            &decoded.projection,
            &rosters,
            None,
            0,
            &mut |bytes| denied_memory.resize(bytes)
        )
        .is_err()
    );
    let evidence = partitioned_codec::PartitionedGenerationEvidenceRefV1::new_charged(
        &decoded.projection,
        &rosters,
        None,
        rosters.retained_bytes(),
        &mut |bytes| memory.resize(bytes),
    )
    .unwrap();
    let mut published = 0;
    let refused = partitioned_codec::write_generation_evidence_charged(
        &evidence,
        0,
        &mut |bytes| denied_memory.resize(bytes),
        &mut |_| {
            published += 1;
            Ok(())
        },
    );
    assert!(matches!(
        refused,
        Err(CodeIndexProductionErrorV1::Publication(
            CodeIndexPublicationStoreErrorV1::ResidentMemoryRefused(_)
        ))
    ));
    assert_eq!(
        published, 0,
        "denied evidence admission must precede the first page and pack commit"
    );
    let mut pages = Vec::new();
    let generation_evidence = partitioned_codec::write_generation_evidence_charged(
        &evidence,
        rosters
            .retained_bytes()
            .saturating_add(evidence.retained_bytes()),
        &mut |bytes| memory.resize(bytes),
        &mut |publication| {
            if let SealedGenerationSegmentPublicationV1::GenerationEvidencePage { bytes, .. } =
                publication
            {
                pages.extend_from_slice(bytes);
            }
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(pages, serde_json::to_vec(&evidence).unwrap());
    assert_eq!(
        generation_evidence.segment_digest,
        ManifestDigest::from_sha256_bytes(&sha2::Sha256::digest(&pages)).unwrap()
    );

    let parsed = partitioned_codec::parse_partitioned_manifest(built.manifest_bytes()).unwrap();
    let generation_ref = partitioned_codec::PartitionedPublishedGenerationRefV1 {
        format_revision: SEALED_GENERATION_FORMAT_REVISION_V1,
        manifest: &parsed.manifest,
        snapshot: &parsed.snapshot,
        statistics: &parsed.statistics,
        chunk_count: parsed.chunk_count,
        chunk_policy: &parsed.chunk_policy,
        repository_parse_identity: &parsed.repository_parse_identity,
        ignored_source_admissions: &parsed.ignored_source_admissions,
        ignored_source_admissions_digest: &parsed.ignored_source_admissions_digest,
        file_segments: &parsed.file_segments,
        file_evidence: &parsed.file_evidence,
        coverage: parsed.coverage,
        capability: &parsed.capability,
        generation_evidence: &parsed.generation_evidence,
        code_graph_pages: &parsed.code_graph_pages,
        resolution_index: &parsed.resolution_index,
    };
    assert!(
        partitioned_codec::seal_partitioned_manifest_charged(&generation_ref, 0, &mut |bytes| {
            denied_memory.resize(bytes)
        })
        .is_err()
    );
    let raw_length = partitioned_codec::encoded_json_rows_length(&generation_ref).unwrap();
    let envelope_ledger = construction_ledger((raw_length + 71) as u64);
    let mut envelope_memory = ConstructionMemoryV1::new(envelope_ledger.begin_operation()).unwrap();
    let envelope_denied =
        partitioned_codec::seal_partitioned_manifest_charged(&generation_ref, 0, &mut |bytes| {
            envelope_memory.resize(bytes)
        });
    assert!(matches!(
        envelope_denied,
        Err(CodeIndexProductionErrorV1::Publication(
            CodeIndexPublicationStoreErrorV1::ResidentMemoryRefused(_)
        ))
    ));
    assert_eq!(
        envelope_memory.charged_bytes,
        (raw_length + 71) as u64,
        "the raw manifest must be admitted before the overlapping envelope is refused"
    );
    drop(envelope_memory);
    assert_eq!(envelope_ledger.snapshot().used_bytes, 0);
    let encoded =
        partitioned_codec::seal_partitioned_manifest_charged(&generation_ref, 0, &mut |bytes| {
            memory.resize(bytes)
        })
        .unwrap();
    let raw = serde_json::to_vec(&generation_ref).unwrap();
    let digest = ManifestDigest::from_sha256_bytes(&sha2::Sha256::digest(&raw)).unwrap();
    let expected = format!(
        "{{\"state_digest\":{},\"generation\":{}}}",
        serde_json::to_string(&digest).unwrap(),
        String::from_utf8(raw).unwrap()
    );
    assert_eq!(
        encoded,
        expected.as_bytes(),
        "the pre-admission codec keeps the established envelope and state hash"
    );
    assert_eq!(encoded, built.manifest_bytes().as_ref());
    drop(evidence);
    drop(rosters);
    drop(memory);
    drop(denied_memory);
    assert_eq!(ledger.snapshot().used_bytes, 0);
    assert_eq!(denied_ledger.snapshot().used_bytes, 0);
}

#[test]
fn cold_publication_reports_computed_clone_payloads() {
    let source = b"pub fn transform(input: u32) -> u32 { let doubled = input * 2; let shifted = doubled + 3; shifted * shifted + doubled }\n";
    let mut owner =
        CodeIndexProductionOwnerV1::new(worker_config(), WorkerPublicationStore::default())
            .expect("production owner");
    let built = owner
        .build_and_publish(
            worker_request_with_source("file.worker.clone-statistics", 1_100_000, source),
            &UninterruptibleCodeIndexControlV1,
        )
        .expect("cold publication");
    let decoded = decoded_generation(&built);
    assert!(!decoded.files[0].artifacts.clone_bodies.is_empty());
    assert_eq!(built.publication().clone_update_statistics(), (0, 0, true));
}

#[test]
fn fresh_generation_resolves_seal_references_once() {
    let mut owner =
        CodeIndexProductionOwnerV1::new(worker_config(), WorkerPublicationStore::default())
            .expect("production owner");
    super::helpers::take_seal_reference_resolutions();

    owner
        .build_and_publish(
            worker_request_with_source(
                "file.worker.resolve-once",
                1_100_000,
                b"pub fn caller() { target(); }\npub fn target() {}\n",
            ),
            &UninterruptibleCodeIndexControlV1,
        )
        .expect("fresh generation");

    assert_eq!(super::helpers::take_seal_reference_resolutions(), 1);
}

/// A generation sealed by one build is reused by the next when only inputs
/// that do not shape stored bytes differ, and only a stored-shape authority
/// retires it. Upgrades restart the daemon, so this is what keeps a sealed
/// generation serving across `tracedecay update`.
#[test]
fn sealed_generation_is_reusable_by_a_later_build_with_the_same_stored_shape() {
    let mut sealing_build =
        CodeIndexProductionOwnerV1::new(worker_config(), WorkerPublicationStore::default())
            .expect("sealing owner");
    let published = sealing_build
        .build_and_publish(
            worker_request_with_source(
                "file.worker.cross-build",
                1_100_000,
                b"pub fn caller() { target(); }\npub fn target() {}\n",
            ),
            &UninterruptibleCodeIndexControlV1,
        )
        .expect("sealed generation");
    let (manifest, segments) = partitioned_seal(&decoded_generation(&published));
    drop(sealing_build);

    let restored = partitioned_restore(&manifest, &segments);
    let later_build = CodeIndexProductionConfigV1 {
        max_snapshot_age_micros: Some(60_000_000),
        ..worker_config()
    };
    let compatibility = restored.compatibility_with(&later_build);
    assert!(
        compatibility.is_reusable(),
        "intake policy is not stored shape: {:?}",
        compatibility.incompatibilities()
    );

    let rechunked = restored.compatibility_with(&CodeIndexProductionConfigV1 {
        chunker_revision: worker_id::<ChunkerRevision>("chunker.v3"),
        ..later_build.clone()
    });
    assert_eq!(
        rechunked
            .incompatibilities()
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        [CodeIndexGenerationIncompatibilityV1::ChunkerRevision]
    );
    assert!(
        rechunked.may_serve_while_rebuilding(),
        "a chunker-only change keeps the sealed bytes serving until the successor is ready"
    );

    let resanitized = restored.compatibility_with(&CodeIndexProductionConfigV1 {
        sanitizer_revision: worker_id::<SanitizerRevision>("sanitizer.v2"),
        ..later_build
    });
    assert!(!resanitized.is_reusable());
    assert!(
        !resanitized.may_serve_while_rebuilding(),
        "bytes sanitized under another revision must not serve"
    );
}

/// Keep the real store's preparation, reads, verification, and CAS. The test
/// rejects only attempts to restage already authenticated reusable segments.
#[derive(Default)]
struct ParentReuseStore {
    inner: WorkerPublicationStore,
    unchanged_file: Option<ManifestDigest>,
    parent_manifest: Option<Arc<[u8]>>,
}

struct ParentReusePreparation {
    inner: MemoryCodeIndexPreparationV1,
    unchanged_file: Option<ManifestDigest>,
    parent_segments: partitioned_codec::ParentSegmentReuseRosterV1,
}

impl CodeIndexPreparationPort for ParentReusePreparation {
    fn write_segment(
        &mut self,
        publication: SealedGenerationSegmentPublicationV1<'_>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        if self.parent_segments.contains(publication) {
            return Err(CodeIndexPublicationStoreErrorV1::Unavailable(
                "authenticated parent evidence, graph, and index segments must not be staged again"
                    .to_owned(),
            ));
        }
        if let SealedGenerationSegmentPublicationV1::File { digest, .. } = publication
            && self.unchanged_file.as_ref() == Some(digest)
        {
            return Err(CodeIndexPublicationStoreErrorV1::Unavailable(
                "unchanged parent file must not be encoded and staged again".to_owned(),
            ));
        }
        self.inner.write_segment(publication)
    }

    fn segment_reader(&self) -> Arc<SealedSegmentReaderV1> {
        self.inner.segment_reader()
    }

    fn abort(self) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        self.inner.abort()
    }
}

impl CodeIndexAtomicPublicationPort for ParentReuseStore {
    type Preparation = ParentReusePreparation;

    fn load_active(
        &self,
        scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<CodeIndexSealedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        self.inner.load_active(scope)
    }

    fn prepare(
        &mut self,
        scope: &CodeIndexGenerationScopeV1,
        expected: Option<&CodeGenerationId>,
    ) -> Result<Self::Preparation, CodeIndexPublicationStoreErrorV1> {
        let parent = self
            .parent_manifest
            .as_ref()
            .map(|bytes| partitioned_codec::parse_partitioned_manifest(bytes))
            .transpose()
            .map_err(|error| CodeIndexPublicationStoreErrorV1::Unavailable(error.to_string()))?;
        let parent_segments =
            partitioned_codec::ParentSegmentReuseRosterV1::new(parent.as_ref(), &mut |_| Ok(()))
                .map_err(|error| {
                    CodeIndexPublicationStoreErrorV1::Unavailable(error.to_string())
                })?;
        Ok(ParentReusePreparation {
            inner: self.inner.prepare(scope, expected)?,
            unchanged_file: self.unchanged_file.clone(),
            parent_segments,
        })
    }

    fn commit(
        &mut self,
        preparation: &mut Self::Preparation,
        publication: &CodeIndexSealedPublicationV1,
    ) -> Result<Arc<[u8]>, CodeIndexPublicationStoreErrorV1> {
        self.inner.commit(&mut preparation.inner, publication)
    }
}

fn parent_reuse_request(large: &[u8], edited: &[u8], sealed_at: i64) -> CodeIndexBuildRequestV1 {
    let mut request = worker_request_with_source("file.worker.large", sealed_at, large);
    request.resident_memory = construction_ledger(64 * 1024 * 1024);
    request.snapshot.files[0].logical_path = "src/large.rs".to_owned();
    let other = worker_request_with_source("file.worker.edited", sealed_at, edited);
    let mut edited_file = other.snapshot.files[0].clone();
    edited_file.logical_path = "src/edited.rs".to_owned();
    request.snapshot.files.push(edited_file);
    request
        .snapshot
        .files
        .sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
    request.snapshot.content_identity =
        worker_id(canonical_sha256(&request.snapshot.files).unwrap().as_str());
    request.captured_files.extend(other.captured_files);
    request.changed_files.insert("src/edited.rs".to_owned());
    request
}

#[test]
fn cold_changed_share_reuses_parent_files_without_restaging_and_keeps_canonical_evidence() {
    let large = (0..128).map(|index| {
        format!("// unchanged source padding xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\npub fn unchanged_{index}() -> usize {{ {index} }}\n")
    }).collect::<String>();
    let mut owner =
        CodeIndexProductionOwnerV1::new(worker_config(), ParentReuseStore::default()).unwrap();
    let first = owner
        .build_and_publish(
            parent_reuse_request(
                large.as_bytes(),
                b"pub fn edited() -> usize { 1 }\n",
                1_100_000,
            ),
            &UninterruptibleCodeIndexControlV1,
        )
        .unwrap();
    let parent = SealedParentGenerationV1::open(first.sealed_generation().clone()).unwrap();
    let large_descriptor = parent.file_segment("src/large.rs").unwrap().clone();
    owner.publication_mut().unchanged_file = Some(large_descriptor.segment_digest.clone());
    owner.publication_mut().parent_manifest = Some(Arc::clone(first.manifest_bytes()));
    let next = owner
        .build_and_publish(
            parent_reuse_request(
                large.as_bytes(),
                b"pub fn edited() -> usize { 2 }\n",
                1_200_000,
            ),
            &UninterruptibleCodeIndexControlV1,
        )
        .expect("cold fallback must carry the existing large file without staging it");
    assert_eq!(
        next.cold_reason(),
        Some(CodeIndexColdBuildReasonV1::ChangedShare)
    );
    let next_parent = SealedParentGenerationV1::open(next.sealed_generation().clone()).unwrap();
    assert_eq!(
        next_parent.file_segment("src/large.rs").unwrap(),
        &large_descriptor
    );
    assert_ne!(
        next_parent
            .file_segment("src/edited.rs")
            .unwrap()
            .segment_digest,
        parent.file_segment("src/edited.rs").unwrap().segment_digest
    );
    let restored = decoded_generation(&next);
    assert_eq!(restored.projection(), next.projection());
    restored.validate_fresh().unwrap();
    let reusable =
        partitioned_codec::ParentSegmentReuseRosterV1::new(Some(parent.generation()), &mut |_| {
            Ok(())
        })
        .unwrap();
    let mut staged_files = Vec::new();
    let full_input = restored.encode_partitioned_sealed_with_parent(Some(first.manifest_bytes()), |segment| {
        assert!(!reusable.contains(segment), "complete-input sealing must also suppress the parent's authenticated non-file segments");
        if let SealedGenerationSegmentPublicationV1::File { digest, .. } = segment {
            staged_files.push(digest.clone());
        }
        Ok(())
    }).unwrap();
    assert!(!staged_files.contains(&large_descriptor.segment_digest));
    assert_eq!(
        full_input.as_slice(),
        next.manifest_bytes().as_ref(),
        "cold reuse and complete-input reuse must seal identical descriptors, projection evidence, and hashes"
    );

    let policy = CodeIndexProductionConfigV1 {
        policy_revision: worker_id("policy.v2"),
        ..worker_config()
    };
    let mut policy_owner = CodeIndexProductionOwnerV1::new(
        policy,
        ParentReuseStore {
            inner: owner.publication_mut().inner.clone(),
            unchanged_file: Some(large_descriptor.segment_digest.clone()),
            parent_manifest: None,
        },
    )
    .unwrap();
    let rebuilt = policy_owner
        .build_and_publish(
            parent_reuse_request(
                large.as_bytes(),
                b"pub fn edited() -> usize { 2 }\n",
                1_300_000,
            ),
            &UninterruptibleCodeIndexControlV1,
        )
        .unwrap();
    assert_eq!(
        rebuilt.cold_reason(),
        Some(CodeIndexColdBuildReasonV1::NoParent)
    );
    let rebuilt_parent =
        SealedParentGenerationV1::open(rebuilt.sealed_generation().clone()).unwrap();
    assert_ne!(
        rebuilt_parent
            .file_segment("src/large.rs")
            .unwrap()
            .segment_digest,
        large_descriptor.segment_digest,
        "a policy mismatch must not carry the old file segment"
    );
}

#[test]
fn parent_file_reuse_requires_snapshot_parser_and_symbol_identity_evidence_before_admission() {
    let mut owner =
        CodeIndexProductionOwnerV1::new(worker_config(), WorkerPublicationStore::default())
            .unwrap();
    let published = owner
        .build_and_publish(
            worker_request_with_source(
                "file.worker.reuse-evidence",
                1_100_000,
                b"pub fn target() {}\n",
            ),
            &UninterruptibleCodeIndexControlV1,
        )
        .unwrap();
    let parent = SealedParentGenerationV1::open(published.sealed_generation().clone()).unwrap();
    let descriptor = parent.file_segment("src/lib.rs").unwrap();
    let row = &parent.snapshot().files[descriptor.file_key as usize];
    let artifact = parent.decode_parent_file("src/lib.rs").unwrap();
    let prior = Some((parent.manifest(), row, descriptor));
    let ledger = construction_ledger(1024 * 1024);
    let mut memory = ConstructionMemoryV1::new(ledger.begin_operation()).unwrap();
    let reused = partitioned_codec::reusable_file_segment(
        parent.manifest(),
        row,
        &artifact,
        descriptor.file_key,
        prior,
        &mut |bytes| memory.resize(bytes),
    )
    .unwrap()
    .unwrap();
    assert_eq!(&reused, descriptor);
    assert_eq!(
        ledger.snapshot().used_bytes,
        0,
        "identity-list guards must drop before returning the descriptor"
    );
    let denied_ledger = construction_ledger(1);
    let mut denied = ConstructionMemoryV1::new(denied_ledger.begin_operation()).unwrap();
    assert!(
        partitioned_codec::reusable_file_segment(
            parent.manifest(),
            row,
            &artifact,
            descriptor.file_key,
            prior,
            &mut |bytes| denied.resize(bytes)
        )
        .is_err()
    );

    let mut different_row = row.clone();
    different_row.file_occurrence_id = worker_id("file.worker.another-occurrence");
    assert!(
        partitioned_codec::reusable_file_segment(
            parent.manifest(),
            &different_row,
            &artifact,
            descriptor.file_key,
            prior,
            &mut |bytes| denied.resize(bytes)
        )
        .unwrap()
        .is_none()
    );
    let mut different_parser = parent.manifest().clone();
    different_parser.extractor_revisions[0].1 = worker_id("extractor.rust.v3");
    assert!(
        partitioned_codec::reusable_file_segment(
            &different_parser,
            row,
            &artifact,
            descriptor.file_key,
            prior,
            &mut |bytes| denied.resize(bytes)
        )
        .unwrap()
        .is_none()
    );
    let mut different_artifact = (*artifact).clone();
    let mut symbol = (*different_artifact.artifacts.symbols[0]).clone();
    symbol.identity = worker_id(&format!("sha256:{}", "f".repeat(64)));
    different_artifact.artifacts.symbols[0] = Arc::new(symbol);
    assert!(
        partitioned_codec::reusable_file_segment(
            parent.manifest(),
            row,
            &different_artifact,
            descriptor.file_key,
            prior,
            &mut |bytes| memory.resize(bytes)
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(ledger.snapshot().used_bytes, 0);
    assert!(
        partitioned_codec::ParentSegmentReuseRosterV1::new(
            Some(parent.generation()),
            &mut |bytes| denied.resize(bytes)
        )
        .is_err()
    );
    let roster = partitioned_codec::ParentSegmentReuseRosterV1::new(
        Some(parent.generation()),
        &mut |bytes| memory.resize(bytes),
    )
    .unwrap();
    assert!(
        ledger.snapshot().used_bytes > 0,
        "the parent digest roster must hold its admitted storage"
    );
    let evidence = &parent.generation().generation_evidence;
    assert!(
        !roster.contains(
            SealedGenerationSegmentPublicationV1::GenerationEvidenceCommit {
                segment_digest: &evidence.segment_digest,
                segment_size_bytes: evidence.segment_size_bytes,
                page_count: evidence.pages.len().try_into().unwrap(),
            }
        ),
        "a parent evidence pack must never suppress a new generation evidence commit"
    );
    drop(roster);
    memory.resize(0).unwrap();
    assert_eq!(ledger.snapshot().used_bytes, 0);
}

/// An unchanged tree over its sealed parent seals over it: no file is
/// extracted again and every segment and graph page carries.
#[test]
fn unchanged_successor_carries_every_parent_segment() {
    let store = WorkerPublicationStore::default();
    let mut owner =
        CodeIndexProductionOwnerV1::new(worker_config(), store.clone()).expect("production owner");
    let source = b"pub fn unchanged() -> u32 { 1 }\n";
    let first = owner
        .build_and_publish(
            worker_request_with_source("file.worker.shared-symbol", 1_100_000, source),
            &UninterruptibleCodeIndexControlV1,
        )
        .expect("first generation");
    let extractions = owner.retained_parse_stats().full_extractions;
    let next = owner
        .build_and_publish(
            worker_request_with_source("file.worker.shared-symbol", 1_200_000, source),
            &UninterruptibleCodeIndexControlV1,
        )
        .expect("unchanged successor");

    assert_eq!(next.cold_reason(), None);
    assert_eq!(next.lane_digest(), first.lane_digest());
    assert_eq!(owner.retained_parse_stats().full_extractions, extractions);
    assert_eq!(
        next.projection().request().changes.reused_count,
        first.projection().request().changes.added_or_changed.len() as u64
    );
    store
        .decode_active(&CodeIndexGenerationScopeV1::for_snapshot(next.snapshot()))
        .expect("successor restores")
        .expect("active successor")
        .validate_fresh()
        .expect("restored successor re-validates without a live parent");
}

#[test]
fn extractor_revision_change_reextracts_before_validating_retained_import_rows() {
    let store = WorkerPublicationStore::default();
    let mut seed = CodeIndexProductionOwnerV1::new(worker_config(), store.clone())
        .expect("seed production owner");
    let first = seed
        .build_and_publish(
            worker_request("file.worker.v4", 1_100_000),
            &UninterruptibleCodeIndexControlV1,
        )
        .expect("seed generation");
    drop(first);
    drop(seed);

    let historical_import_digest =
        canonical_sha256(&"historical import row schema").expect("historical row digest");
    let scope =
        CodeIndexGenerationScopeV1::for_snapshot(&worker_request("file.worker.v4", 0).snapshot);
    {
        let mut active = store
            .decode_active(&scope)
            .expect("seed decodes")
            .expect("seeded active generation");
        let incumbent = active.manifest.generation_id.clone();
        let rust = worker_id::<LanguageId>("rust");
        let mut descriptor = StaticLanguageRegistry::new()
            .descriptor(&rust)
            .expect("compiled Rust descriptor")
            .clone();
        descriptor.extractor_revision =
            ExtractorRevision::new("extractor.rust.v3").expect("historical extractor revision");
        let historical_registry = StaticLanguageRegistry::try_from_descriptors(vec![descriptor])
            .expect("historical registry");
        active.manifest.registry_revision = historical_registry.registry_revision();
        active.manifest.extractor_revisions = historical_registry
            .descriptors()
            .into_iter()
            .map(|descriptor| {
                (
                    descriptor.language.clone(),
                    descriptor.extractor_revision.clone(),
                )
            })
            .collect();
        active.manifest.seal.expected_digest =
            expected_seal_digest(&active.manifest).expect("reseal historical manifest");

        active.validated = OnceLock::new();
        let mut publisher = store.clone();
        let mut preparation = publisher
            .prepare(&scope, Some(&incumbent))
            .expect("historical preparation");
        let publication = active
            .stage_publication(&mut preparation)
            .expect("historical generation stages through the canonical codec");
        publisher
            .commit(&mut preparation, &publication)
            .expect("historical generation seals");
    }

    let mut upgraded =
        CodeIndexProductionOwnerV1::new(worker_config(), store).expect("upgraded production owner");
    let rebuilt = upgraded
        .build_and_publish(
            worker_request("file.worker.v4", 1_200_000),
            &UninterruptibleCodeIndexControlV1,
        )
        .expect("extractor revision change re-extracts from source");

    assert_eq!(
        rebuilt.cold_reason(),
        Some(CodeIndexColdBuildReasonV1::NoParent)
    );
    let rebuilt = decoded_generation(&rebuilt);
    assert_eq!(
        rebuilt.files[0].extraction.extractor_revision.as_str(),
        "extractor.rust.v21"
    );
    assert_ne!(
        rebuilt.files[0].extraction.parser_import_rows_digest,
        historical_import_digest
    );
    assert_eq!(upgraded.retained_parse_stats().full_extractions, 1);
}

/// Retain the output of the same intake, planning and extraction path the
/// constructor stages one window at a time. The test is the explicit strong
/// owner; the physical pool remains only its weak index.
fn retained_worker_artifact(
    request: &CodeIndexBuildRequestV1,
    pool: &SharedPhysicalCodeArtifactPoolV1,
) -> (ManifestDigest, Arc<FileGenerationArtifactsV1>) {
    let config = worker_config();
    let intake = SanitizedCodeIntake::new(
        StaticLanguageRegistry::new(),
        config.sanitizer_revision.clone(),
        request.sealed_at,
    );
    let capability = intake
        .admit(request.snapshot.clone())
        .expect("artifact intake");
    let planner = GenerationPlanner::new(
        config.project_id.clone(),
        config.repository.clone(),
        StaticLanguageRegistry::new(),
        config.chunker_revision.clone(),
        config.privacy_domain.clone(),
        config.privacy_key_epoch,
    );
    let manifest = planner
        .plan_generation_with_invalidation(
            capability.snapshot(),
            None,
            &request.invalidations,
            request.sealed_at,
        )
        .expect("artifact generation plan");
    let chunker = DeterministicCodeChunker::new(
        manifest.generation_id.clone(),
        config.repository.clone(),
        config.sanitizer_revision.clone(),
        config.policy_revision.clone(),
        config.chunker_revision.clone(),
    );
    let captured = request
        .captured_files
        .iter()
        .cloned()
        .map(|file| (file.file_occurrence_id.clone(), file))
        .collect();
    let (key, artifact, _) = materialize_present_file(
        &config,
        pool,
        &SharedRetainedParsePool::default(),
        &intake,
        &capability,
        &manifest,
        &TreeSitterExtractor::new(),
        &chunker,
        &request.repository_parse_identity,
        None,
        &request.snapshot.files[0],
        &captured,
        &UninterruptibleCodeIndexControlV1,
    )
    .expect("canonical retained file extraction");
    pool.insert(key.clone(), &artifact);
    (key, artifact)
}

#[test]
fn held_physical_artifacts_share_payloads_and_rebind_without_pinning_the_corpus() {
    let source = b"pub fn transform(input: u32) -> u32 {\n    let doubled = input * 2;\n    let shifted = doubled + 3;\n    shifted * shifted + doubled\n}\n";
    let pool = SharedPhysicalCodeArtifactPoolV1::default();
    let seed_request = worker_request_with_source("file.worker.shared", 1_100_000, source);
    let (key, held) = retained_worker_artifact(&seed_request, &pool);
    assert_eq!(pool.stats().resident, 1);
    assert_eq!(pool.stats().inserted, 1);
    assert_eq!(pool.stats().reused, 0);
    assert!(
        !held.artifacts.clone_bodies.is_empty(),
        "the fixture has shared payloads"
    );

    let request = worker_request_with_source("file.worker.shared", 1_200_000, source);
    let borrowed = std::thread::scope(|scope| {
        scope
            .spawn(|| retained_worker_artifact(&request, &pool).1)
            .join()
            .expect("artifact borrower")
    });
    assert_eq!(pool.stats().reused, 1);
    assert_eq!(pool.stats().resident, 1);
    assert_eq!(
        held.artifacts.clone_bodies.len(),
        borrowed.artifacts.clone_bodies.len()
    );
    for (source, rebound) in held
        .artifacts
        .clone_bodies
        .iter()
        .zip(&borrowed.artifacts.clone_bodies)
    {
        assert!(Arc::ptr_eq(&source.payload, &rebound.payload));
    }
    let (_, cold) =
        retained_worker_artifact(&request, &SharedPhysicalCodeArtifactPoolV1::default());
    assert_eq!(borrowed.authority, cold.authority);
    assert_eq!(borrowed.extraction, cold.extraction);
    assert_eq!(
        borrowed.artifacts, cold.artifacts,
        "cache reuse preserves every file artifact"
    );
    borrowed
        .exact_authority
        .validate_all(&borrowed.artifacts.chunks.chunks)
        .expect("borrowed chunks retain parser-backed exact authority");
    cold.exact_authority
        .validate_all(&cold.artifacts.chunks.chunks)
        .expect("cold comparison chunks retain parser-backed exact authority");

    let foreign_id = worker_id::<FileOccurrenceId>("file.worker.foreign");
    let foreign_request = worker_request_with_source(foreign_id.as_str(), 1_200_000, source);
    let (_, foreign) = retained_worker_artifact(&foreign_request, &pool);
    assert_eq!(pool.stats().reused, 2);
    assert_eq!(
        pool.stats().inserted,
        1,
        "occurrence identity is outside the physical key"
    );
    assert_eq!(foreign.extraction.file_occurrence_id, foreign_id);
    assert!(!foreign.artifacts.chunks.chunks.is_empty());
    assert!(
        foreign
            .artifacts
            .chunks
            .chunks
            .iter()
            .all(|chunk| chunk.anchor.file_occurrence_id == foreign_id)
    );
    for (source, rebound) in held
        .artifacts
        .clone_bodies
        .iter()
        .zip(&foreign.artifacts.clone_bodies)
    {
        assert!(Arc::ptr_eq(&source.payload, &rebound.payload));
    }
    let (_, foreign_cold) = retained_worker_artifact(
        &foreign_request,
        &SharedPhysicalCodeArtifactPoolV1::default(),
    );
    assert_eq!(foreign.artifacts, foreign_cold.artifacts);
    foreign
        .exact_authority
        .validate_all(&foreign.artifacts.chunks.chunks)
        .expect("foreign chunks have their own exact authority");
    assert!(
        foreign
            .exact_authority
            .validate_all(&held.artifacts.chunks.chunks)
            .is_err(),
        "foreign exact authority cannot admit the source occurrence's chunks"
    );

    let changed_request = worker_request_with_source(
        "file.worker.changed",
        1_300_000,
        b"pub fn changed() -> u32 { 9 }\n",
    );
    let (_, changed) = retained_worker_artifact(&changed_request, &pool);
    assert_eq!(
        changed.authority.content_digest,
        changed_request.snapshot.files[0].content_digest
    );
    assert_ne!(
        changed.authority.content_digest,
        held.authority.content_digest
    );
    assert_eq!(
        pool.stats().reused,
        2,
        "different source bytes cannot hit the old artifact"
    );
    assert_eq!(pool.stats().inserted, 2);
    let mut offpath_request = worker_request_with_source("file.worker.offpath", 1_300_000, source);
    offpath_request.snapshot.files[0].logical_path = "src/other.rs".to_owned();
    let (_, offpath) = retained_worker_artifact(&offpath_request, &pool);
    assert_eq!(offpath.authority.logical_path, "src/other.rs");
    assert_eq!(
        pool.stats().reused,
        2,
        "identical bytes at another path cannot alias the artifact"
    );
    assert_eq!(pool.stats().inserted, 3);

    // Retain a real source artifact while a complete production build borrows
    // it; compare that build with an independent cold build of the same input.
    pool.insert(key, &held);
    let mut owner =
        CodeIndexProductionOwnerV1::new(worker_config(), WorkerPublicationStore::default())
            .expect("borrowing owner")
            .with_physical_artifact_pool(pool.clone());
    let reused = owner
        .build_and_publish(request, &UninterruptibleCodeIndexControlV1)
        .expect("production build borrows held artifact");
    assert_eq!(pool.stats().reused, 3);
    assert_eq!(owner.retained_parse_stats().full_extractions, 0);
    let cold = CodeIndexProductionOwnerV1::new(worker_config(), WorkerPublicationStore::default())
        .expect("cold owner")
        .build_and_publish(
            worker_request_with_source("file.worker.shared", 1_200_000, source),
            &UninterruptibleCodeIndexControlV1,
        )
        .expect("independent cold build");
    assert_eq!(reused.manifest_bytes(), cold.manifest_bytes());
    assert_eq!(
        partitioned_seal(&decoded_generation(&reused)),
        partitioned_seal(&decoded_generation(&cold)),
        "a held artifact hit preserves every sealed byte and chunk identity",
    );

    drop((held, borrowed, foreign, changed, offpath));
    assert_eq!(
        pool.stats().resident,
        0,
        "the weak pool never extends an artifact's lifetime"
    );
    pool.release_dead_entries();
    assert_eq!(pool.stats().resident, 0);
}

#[test]
fn physical_artifact_reuse_rejects_a_stale_extractor_revision() {
    let source = b"mod inner { pub fn value() {} }\npub use inner::*;\n";
    let pool = SharedPhysicalCodeArtifactPoolV1::default();
    let mut seed =
        CodeIndexProductionOwnerV1::new(worker_config(), WorkerPublicationStore::default())
            .expect("seed production owner")
            .with_physical_artifact_pool(pool.clone());
    let generation = seed
        .build_and_publish(
            worker_request_with_source("file.worker.pool", 1_100_000, source),
            &UninterruptibleCodeIndexControlV1,
        )
        .expect("seed generation");
    let mut stale = decoded_generation(&generation).files[0].as_ref().clone();
    stale.extraction.extractor_revision =
        ExtractorRevision::new("extractor.rust.v3").expect("historical extractor revision");
    stale.extraction.parser_import_rows_digest =
        canonical_sha256(&"historical import row schema").expect("historical row digest");
    stale.artifacts.imports.clear();
    let stale = Arc::new(stale);

    let request = worker_request_with_source("file.worker.pool", 1_200_000, source);
    let language = request.snapshot.files[0]
        .language
        .as_ref()
        .expect("Rust language");
    let registry = StaticLanguageRegistry::new();
    let descriptor = registry
        .descriptor(language)
        .expect("compiled Rust descriptor");
    let reuse_key = physical_reuse_key(
        &worker_config(),
        &request.snapshot.files[0],
        descriptor,
        SensitivityLevelV1::Public,
    )
    .expect("physical reuse key");
    pool.insert(reuse_key, &stale);

    let mut upgraded =
        CodeIndexProductionOwnerV1::new(worker_config(), WorkerPublicationStore::default())
            .expect("upgraded production owner")
            .with_physical_artifact_pool(pool);
    let rebuilt = upgraded
        .build_and_publish(request, &UninterruptibleCodeIndexControlV1)
        .expect("stale pooled artifact is re-extracted");
    let rebuilt = decoded_generation(&rebuilt);

    assert_eq!(
        rebuilt.files[0].extraction.extractor_revision.as_str(),
        "extractor.rust.v21"
    );
    assert!(
        rebuilt.files[0]
            .artifacts
            .imports
            .iter()
            .any(|row| row.is_public && row.is_glob),
        "replacement import evidence must have the v4 public-glob shape"
    );
}

#[test]
fn prior_sealed_generation_is_rejected_before_manifest_decode() {
    let refuse = |revision: u32| {
        let generation = format!(r#"{{"format_revision":{revision}}}"#);
        let digest =
            ManifestDigest::from_sha256_bytes(&sha2::Sha256::digest(generation.as_bytes()))
                .expect("generation digest");
        let envelope = format!(
            r#"{{"state_digest":"{}","generation":{generation}}}"#,
            digest.as_str()
        );
        CodeIndexPublishedGenerationV1::decode_partitioned_sealed(
            envelope.as_bytes(),
            &SharedDecodedContentPoolV1::default(),
            |_, _| panic!("a revision gate must refuse before any segment read"),
        )
        .expect_err("an incomplete envelope cannot decode")
    };

    let current = SEALED_GENERATION_FORMAT_REVISION_V1;
    let current_error = refuse(current);
    assert!(
        matches!(
            current_error,
            CodeIndexProductionErrorV1::Contract(ref message)
                if message.contains("payload decoding failed")
        ),
        "the current revision must pass the revision gate and reach payload decode: {current_error}"
    );

    let previous = current - 1;
    let previous_error = refuse(previous);
    assert!(
        matches!(
            previous_error,
            CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(refused)
                if refused == previous
        ),
        "the previous revision reached the wrong rejection: {previous_error}"
    );
    assert!(
        previous_error
            .to_string()
            .contains("will be rebuilt from source")
    );
}

/// Seal `generation` partitioned, keeping every segment in memory with the
/// evidence pages assembled under their pack digest.
pub(super) fn partitioned_seal(
    generation: &CodeIndexPublishedGenerationV1,
) -> (Vec<u8>, std::collections::BTreeMap<String, Vec<u8>>) {
    let mut segments = std::collections::BTreeMap::new();
    let mut evidence_pack = Vec::new();
    let manifest = generation
        .encode_partitioned_sealed(|publication| {
            match publication {
                SealedGenerationSegmentPublicationV1::File { digest, bytes }
                | SealedGenerationSegmentPublicationV1::FileEvidence { digest, bytes }
                | SealedGenerationSegmentPublicationV1::ResolutionIndex { digest, bytes } => {
                    segments.insert(digest.as_str().to_owned(), bytes.to_vec());
                }
                SealedGenerationSegmentPublicationV1::CodeGraphPage {
                    page_digest, bytes, ..
                } => {
                    segments.insert(page_digest.as_str().to_owned(), bytes.to_vec());
                }
                SealedGenerationSegmentPublicationV1::GenerationEvidencePage { bytes, .. } => {
                    evidence_pack.extend_from_slice(bytes);
                }
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
        .expect("generation seals");
    (manifest, segments)
}

pub(super) fn partitioned_restore(
    manifest: &[u8],
    segments: &std::collections::BTreeMap<String, Vec<u8>>,
) -> CodeIndexPublishedGenerationV1 {
    CodeIndexPublishedGenerationV1::decode_partitioned_sealed(
        manifest,
        &SharedDecodedContentPoolV1::default(),
        |request, buffer| {
            let (digest, offset, length) = match request {
                SealedGenerationSegmentReadV1::Whole { digest, size_bytes } => {
                    (digest, 0, size_bytes)
                }
                SealedGenerationSegmentReadV1::Range {
                    digest,
                    offset,
                    length,
                    ..
                } => (digest, offset, length),
            };
            let bytes = &segments[digest.as_str()];
            let start = usize::try_from(offset).expect("segment offset");
            let end = start + usize::try_from(length).expect("segment length");
            buffer.clear();
            buffer.extend_from_slice(&bytes[start..end]);
            Ok(())
        },
    )
    .expect("generation restores")
}

#[test]
fn parallel_collection_returns_the_lowest_index_failure() {
    let visited = AtomicUsize::new(0);
    let items = (0..256_usize).collect::<Vec<_>>();

    let error = collect_bounded_ordered(&items, |item| {
        visited.fetch_add(1, Ordering::Relaxed);
        if *item == 2 || *item == 200 {
            Err(WorkerTestError::Mapping(*item))
        } else {
            Ok(*item)
        }
    })
    .expect_err("the mapping fails");

    assert_eq!(
        error,
        WorkerTestError::Mapping(2),
        "the reported failure must be the sequential one, not the first to finish"
    );
    assert!(visited.load(Ordering::Relaxed) > 0);
}

/// One malformed source file must not take the whole generation down with it.
/// A panicking per-file unit is contained and reported as that unit's typed
/// failure; every other file still runs to completion.
#[test]
fn parallel_collection_contains_a_panicking_unit_without_poisoning_the_rest() {
    let completed = AtomicUsize::new(0);
    let items = (0..256_usize).collect::<Vec<_>>();

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let outcome = collect_bounded_ordered(&items, |item| {
        if *item == 200 {
            panic!("synthetic per-file panic");
        }
        completed.fetch_add(1, Ordering::Relaxed);
        Ok::<_, WorkerTestError>(*item)
    });
    std::panic::set_hook(previous_hook);

    let error = outcome.expect_err("a panicking unit must surface as a failure");
    assert_eq!(
        error,
        WorkerTestError::Parallelism(
            crate::parallelism::CodeIndexParallelismErrorV1::WorkerPanic {
                index: 200,
                message: "synthetic per-file panic".to_owned(),
            }
        ),
        "the panic must be reported as that unit's typed failure"
    );
    assert_eq!(
        completed.load(Ordering::Relaxed),
        items.len() - 1,
        "every non-panicking unit must still complete"
    );
}

/// A panic in a later unit must not mask an ordinary failure in an earlier
/// one: reported failure stays the lowest-index one, panic or not.
#[test]
fn parallel_collection_reports_the_lowest_index_failure_across_panics() {
    let items = (0..256_usize).collect::<Vec<_>>();

    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let error = collect_bounded_ordered(&items, |item| {
        assert_ne!(*item, 200, "synthetic per-file panic");
        if *item == 2 {
            Err(WorkerTestError::Mapping(*item))
        } else {
            Ok(*item)
        }
    })
    .expect_err("the mapping fails");
    std::panic::set_hook(previous_hook);

    assert_eq!(error, WorkerTestError::Mapping(2));
}

#[test]
fn concurrent_attribution_reads_share_success_without_blocking_cached_reads() {
    let mut owner =
        CodeIndexProductionOwnerV1::new(worker_config(), WorkerPublicationStore::default())
            .unwrap();
    let published = owner.build_and_publish(
        worker_request_with_source(
            "file.worker.attribution", 1_100_000,
            b"fn target() {}\n#[test] fn first() { target(); }\n#[test] fn second() { target(); }\n",
        ),
        &UninterruptibleCodeIndexControlV1,
    ).unwrap();
    let generation = decoded_generation(&published);
    let cloned_generation = (*generation).clone();
    assert_eq!(
        cloned_generation.test_attribution_read().provider_state,
        ProviderEvaluationStateV1::Indexing
    );
    let start = std::sync::Barrier::new(4);
    let reads = std::thread::scope(|scope| {
        let workers = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    start.wait();
                    generation
                        .prepare_test_attribution(&UninterruptibleCodeIndexControlV1)
                        .unwrap()
                        .read_test_attribution(&generation.manifest().generation_id)
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(reads.iter().all(|read| Arc::ptr_eq(&reads[0], read)));
    let _building = generation.attribution_build.lock().unwrap();
    assert!(generation.retained_bytes() > 0);
    let cached = generation
        .prepare_test_attribution(&UninterruptibleCodeIndexControlV1)
        .unwrap()
        .read_test_attribution(&generation.manifest().generation_id);
    assert!(Arc::ptr_eq(&reads[0], &cached));
    assert!(Arc::ptr_eq(
        &cached,
        &cloned_generation.test_attribution_read()
    ));
    assert_eq!(
        cloned_generation.manifest().generation_id,
        generation.manifest().generation_id
    );
}

#[test]
fn failed_attribution_build_can_retry_and_resident_accounting_never_waits_for_it() {
    let mut owner =
        CodeIndexProductionOwnerV1::new(worker_config(), WorkerPublicationStore::default())
            .unwrap();
    let published = owner
        .build_and_publish(
            worker_request_with_source(
                "file.worker.attribution-retry",
                1_100_000,
                b"fn target() {}\n#[test] fn test() { target(); }\n",
            ),
            &UninterruptibleCodeIndexControlV1,
        )
        .unwrap();
    let mut generation = (*decoded_generation(&published)).clone();
    let files = std::mem::take(&mut generation.snapshot.files);
    assert!(
        generation
            .prepare_test_attribution(&UninterruptibleCodeIndexControlV1)
            .is_err()
    );
    assert!(generation.attribution.get().is_none());
    assert_eq!(
        generation.test_attribution_read().provider_state,
        ProviderEvaluationStateV1::Failed
    );
    {
        let _building = generation.attribution_build.lock().unwrap();
        assert!(generation.retained_bytes() > 0);
        let warming = generation.test_attribution_read();
        assert_eq!(warming.provider_state, ProviderEvaluationStateV1::Indexing);
        assert!(warming.evidence.is_none());
    }
    generation.snapshot.files = files;
    assert!(
        generation
            .prepare_test_attribution(&UninterruptibleCodeIndexControlV1)
            .is_ok()
    );
    assert!(generation.attribution.get().is_some());
}

#[test]
fn attribution_preparation_checks_cancellation_and_can_retry() {
    struct CancelAfterChecks(std::sync::atomic::AtomicUsize);
    impl CodeIndexExecutionControlV1 for CancelAfterChecks {
        fn is_cancelled(&self) -> bool {
            self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed) >= 8
        }
        fn is_deadline_exceeded(&self) -> bool {
            false
        }
    }
    let mut owner =
        CodeIndexProductionOwnerV1::new(worker_config(), WorkerPublicationStore::default())
            .unwrap();
    let published = owner.build_and_publish(worker_request_with_source(
        "file.worker.attribution-cancel", 1_100_000,
        b"fn target() {}\n#[test] fn first() { target(); }\n#[test] fn second() { first(); }\n",
    ), &UninterruptibleCodeIndexControlV1).unwrap();
    let generation = decoded_generation(&published);
    let control = CancelAfterChecks(std::sync::atomic::AtomicUsize::new(0));
    assert!(matches!(
        generation.prepare_test_attribution(&control),
        Err(CodeIndexProductionErrorV1::Interrupted(
            CodeIndexInterruptionV1::Cancelled
        ))
    ));
    let cancelled = generation.test_attribution_read();
    assert_eq!(
        cancelled.provider_state,
        ProviderEvaluationStateV1::Cancelled
    );
    assert!(cancelled.evidence.is_none());
    assert!(generation.attribution.get().is_none());
    generation
        .prepare_test_attribution(&UninterruptibleCodeIndexControlV1)
        .unwrap();
    assert!(generation.test_attribution_read().evidence.is_some());
}
