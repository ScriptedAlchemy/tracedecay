use std::sync::atomic::{AtomicUsize, Ordering};

use sha2::Digest as _;

use tracedecay_domain::{
    ChunkerRevision, ExtractorRevision, LanguageId, PrivacyDomainId, ProjectionKeyV1,
    ProjectionKindV1, ProjectionOperationV1, ProjectionOutcomeV1, SanitizationReceiptId,
    SanitizerRevision,
};

use crate::projection::{
    ProjectionReceiptBuilderV1, ProjectionSinkErrorV1, ProjectionSinkReceiptV1,
};
use crate::receipts::ChunkProjectionDecisionV1;

use super::*;

#[derive(Clone, Default)]
struct WorkerPublicationStore {
    active: Arc<Mutex<Option<Arc<CodeIndexPublishedGenerationV1>>>>,
}

impl CodeIndexAtomicPublicationPort for WorkerPublicationStore {
    fn load_active(
        &self,
        _scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<Arc<CodeIndexPublishedGenerationV1>>, CodeIndexPublicationStoreErrorV1> {
        Ok(self
            .active
            .lock()
            .expect("publication lock")
            .as_ref()
            .map(Arc::clone))
    }

    fn publish_atomically(
        &mut self,
        _scope: &CodeIndexGenerationScopeV1,
        expected_active_generation: Option<&CodeGenerationId>,
        generation: Arc<CodeIndexPublishedGenerationV1>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let mut active = self.active.lock().expect("publication lock");
        if active
            .as_ref()
            .map(|current| &current.manifest.generation_id)
            != expected_active_generation
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        *active = Some(generation);
        Ok(())
    }
}

struct WorkerProjectionSink;

impl CodeChunkProjectionSink for WorkerProjectionSink {
    fn project_changed_chunks(
        &mut self,
        request: &ProjectionBatchRequestV1,
        receipt_builder: ProjectionReceiptBuilderV1<'_>,
    ) -> Result<ProjectionSinkReceiptV1, ProjectionSinkErrorV1> {
        let decisions = request
            .changes
            .added_or_changed
            .iter()
            .map(|change| ChunkProjectionDecisionV1 {
                chunk_id: change.chunk_id.clone(),
                prior_chunk_digest: change.prior_digest.clone(),
                current_chunk_digest: change.current_digest.clone(),
                operation: if change.prior_digest.is_some() {
                    ProjectionOperationV1::Updated
                } else {
                    ProjectionOperationV1::Added
                },
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: change.current_digest.clone(),
            })
            .collect::<Vec<_>>();
        receipt_builder
            .build(&decisions)
            .map_err(|error| ProjectionSinkErrorV1::Rejected(error.to_string()))
    }
}

fn worker_id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    <T as TryFrom<String>>::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("valid fixture identity")
}

fn worker_config() -> CodeIndexProductionConfigV1 {
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

fn worker_request_with_source(
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

#[test]
fn fresh_generation_resolves_seal_references_once() {
    let mut owner = CodeIndexProductionOwnerV1::new(
        worker_config(),
        WorkerPublicationStore::default(),
        WorkerProjectionSink,
    )
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

/// Restoring a sealed generation resolves its cross-file references once.
///
/// Edges are derived, never persisted, so the restore already owns the only
/// edge vector these files can produce: a second resolution inside validation
/// re-runs the corpus-scale reference walk to compare a deterministic
/// derivation against itself.
#[test]
fn restored_generation_resolves_seal_references_once() {
    let mut owner = CodeIndexProductionOwnerV1::new(
        worker_config(),
        WorkerPublicationStore::default(),
        WorkerProjectionSink,
    )
    .expect("production owner");
    let published = owner
        .build_and_publish(
            worker_request_with_source(
                "file.worker.restore-resolve-once",
                1_100_000,
                b"pub fn caller() { target(); }\npub fn target() {}\n",
            ),
            &UninterruptibleCodeIndexControlV1,
        )
        .expect("fresh generation");
    let (manifest, segments) = partitioned_seal(&published);
    super::helpers::take_seal_reference_resolutions();

    let restored = partitioned_restore(&manifest, &segments);

    assert_eq!(super::helpers::take_seal_reference_resolutions(), 1);
    assert_eq!(restored.edges, published.edges);
}

/// A generation sealed by one build is reused by the next when only inputs
/// that do not shape stored bytes differ, and only a stored-shape authority
/// retires it. Upgrades restart the daemon, so this is what keeps a sealed
/// generation serving across `tracedecay update`.
#[test]
fn sealed_generation_is_reusable_by_a_later_build_with_the_same_stored_shape() {
    let mut sealing_build = CodeIndexProductionOwnerV1::new(
        worker_config(),
        WorkerPublicationStore::default(),
        WorkerProjectionSink,
    )
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
    let (manifest, segments) = partitioned_seal(&published);
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
        rechunked.incompatibilities().iter().copied().collect::<Vec<_>>(),
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

#[test]
fn unchanged_increment_shares_symbol_records_with_parent_generation() {
    let store = WorkerPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(worker_config(), store, WorkerProjectionSink)
        .expect("production owner");
    let source = b"pub fn unchanged() -> u32 { 1 }\n";
    let first = owner
        .build_and_publish(
            worker_request_with_source("file.worker.shared-symbol", 1_100_000, source),
            &UninterruptibleCodeIndexControlV1,
        )
        .expect("first generation");
    let next = owner
        .build_and_publish(
            worker_request_with_source("file.worker.shared-symbol", 1_200_000, source),
            &UninterruptibleCodeIndexControlV1,
        )
        .expect("unchanged increment");

    assert_eq!(first.symbols.symbols.len(), 1);
    assert!(Arc::ptr_eq(
        &first.symbols.symbols[0],
        &next.symbols.symbols[0]
    ));
    assert!(Arc::ptr_eq(
        &first.files[0].artifacts.clone_bodies[0].payload,
        &next.files[0].artifacts.clone_bodies[0].payload
    ));
}

/// Arc-share incremental seals must survive parentless sealed restore.
///
/// Publish validates with a live parent; restore calls `validate_fresh()` with
/// no parent and must still authenticate the Arc-share reused complement.
/// Omit captured bytes on the successor so the file page is Arc-shared.
#[test]
fn arc_share_increment_restores_under_parentless_validate_fresh() {
    let store = WorkerPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(worker_config(), store, WorkerProjectionSink)
        .expect("production owner");
    let source = b"pub fn carried() -> u32 { 7 }\n";
    let first = owner
        .build_and_publish(
            worker_request_with_source("file.worker.arc-share-restore", 1_100_000, source),
            &UninterruptibleCodeIndexControlV1,
        )
        .expect("first generation");
    let mut carry = worker_request_with_source("file.worker.arc-share-restore", 1_200_000, source);
    carry.captured_files.clear();
    let next = owner
        .build_and_publish(carry, &UninterruptibleCodeIndexControlV1)
        .expect("arc-share increment");
    assert!(
        next.projection.request().changes.reused_count > 0,
        "unchanged carry must seal a non-empty reused complement"
    );
    assert!(
        Arc::ptr_eq(&first.files[0], &next.files[0]),
        "fixture must Arc-share the unchanged file page"
    );

    let (manifest, segments) = partitioned_seal(&next);
    let restored = partitioned_restore(&manifest, &segments);
    assert_eq!(restored.manifest.generation_id, next.manifest.generation_id);
    assert_eq!(
        restored.projection.request().changes.reused_digest,
        next.projection.request().changes.reused_digest
    );
    restored
        .validate_fresh()
        .expect("restored generation must re-validate without a live parent");
}

#[test]
fn extractor_revision_change_reextracts_before_validating_retained_import_rows() {
    let store = WorkerPublicationStore::default();
    let mut seed =
        CodeIndexProductionOwnerV1::new(worker_config(), store.clone(), WorkerProjectionSink)
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
    {
        let mut slot = store.active.lock().expect("publication lock");
        let active = Arc::make_mut(slot.as_mut().expect("seeded active generation"));
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

        let file = Arc::make_mut(&mut active.files[0]);
        file.extraction.extractor_revision =
            ExtractorRevision::new("extractor.rust.v3").expect("historical extractor revision");
        file.extraction.parser_import_rows_digest = historical_import_digest.clone();
        active.validated = OnceLock::new();
    }

    let mut upgraded =
        CodeIndexProductionOwnerV1::new(worker_config(), store, WorkerProjectionSink)
            .expect("upgraded production owner");
    let rebuilt = upgraded
        .build_and_publish(
            worker_request("file.worker.v4", 1_200_000),
            &UninterruptibleCodeIndexControlV1,
        )
        .expect("extractor revision change re-extracts from source");

    assert_eq!(
        rebuilt.files[0].extraction.extractor_revision.as_str(),
        "extractor.rust.v13"
    );
    assert_ne!(
        rebuilt.files[0].extraction.parser_import_rows_digest,
        historical_import_digest
    );
    assert_eq!(upgraded.retained_parse_stats().full_extractions, 1);
}

#[test]
fn physical_artifact_reuse_rejects_a_stale_extractor_revision() {
    let source = b"mod inner { pub fn value() {} }\npub use inner::*;\n";
    let pool = SharedPhysicalCodeArtifactPoolV1::default();
    let mut seed = CodeIndexProductionOwnerV1::new(
        worker_config(),
        WorkerPublicationStore::default(),
        WorkerProjectionSink,
    )
    .expect("seed production owner")
    .with_physical_artifact_pool(pool.clone());
    let generation = seed
        .build_and_publish(
            worker_request_with_source("file.worker.pool", 1_100_000, source),
            &UninterruptibleCodeIndexControlV1,
        )
        .expect("seed generation");
    let mut stale = generation.files[0].as_ref().clone();
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
    let reuse_key = CodeIndexProductionOwnerV1::<
        WorkerPublicationStore,
        WorkerProjectionSink,
    >::physical_reuse_key(
        &worker_config(),
        &request.snapshot.files[0],
        descriptor,
        SensitivityLevelV1::Public,
    )
    .expect("physical reuse key");
    pool.insert(reuse_key, &stale);

    let mut upgraded = CodeIndexProductionOwnerV1::new(
        worker_config(),
        WorkerPublicationStore::default(),
        WorkerProjectionSink,
    )
    .expect("upgraded production owner")
    .with_physical_artifact_pool(pool);
    let rebuilt = upgraded
        .build_and_publish(request, &UninterruptibleCodeIndexControlV1)
        .expect("stale pooled artifact is re-extracted");

    assert_eq!(
        rebuilt.files[0].extraction.extractor_revision.as_str(),
        "extractor.rust.v13"
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
fn incremental_carry_forward_rejects_a_stale_extractor_revision() {
    let source = b"mod inner { pub fn value() {} }\npub use inner::*;\n";
    let store = WorkerPublicationStore::default();
    let mut seed =
        CodeIndexProductionOwnerV1::new(worker_config(), store.clone(), WorkerProjectionSink)
            .expect("seed production owner");
    let generation = seed
        .build_and_publish(
            worker_request_with_source("file.worker.increment", 1_100_000, source),
            &UninterruptibleCodeIndexControlV1,
        )
        .expect("seed generation");
    drop(generation);
    drop(seed);

    {
        let mut slot = store.active.lock().expect("publication lock");
        let active = Arc::make_mut(slot.as_mut().expect("seeded active generation"));
        let file = Arc::make_mut(&mut active.files[0]);
        file.extraction.extractor_revision =
            ExtractorRevision::new("extractor.rust.v3").expect("historical extractor revision");
        file.extraction.parser_import_rows_digest =
            canonical_sha256(&"historical import row schema").expect("historical row digest");
        file.artifacts.imports.clear();
        assert!(
            active.validated.get().is_some(),
            "fixture keeps the already-validated generation memo"
        );
    }

    let mut upgraded =
        CodeIndexProductionOwnerV1::new(worker_config(), store, WorkerProjectionSink)
            .expect("upgraded production owner");
    let rebuilt = upgraded
        .build_and_publish(
            worker_request_with_source("file.worker.increment", 1_200_000, source),
            &UninterruptibleCodeIndexControlV1,
        )
        .expect("stale carried artifact is re-extracted");

    assert_eq!(upgraded.retained_parse_stats().full_extractions, 1);
    assert!(
        rebuilt.files[0]
            .artifacts
            .imports
            .iter()
            .any(|row| row.is_public && row.is_glob),
        "replacement import evidence must have the current public-glob shape"
    );
}

#[test]
fn prior_sealed_generation_is_rejected_before_manifest_decode() {
    for revision in [4, 9] {
        let generation = format!(r#"{{"format_revision":{revision}}}"#);
        let digest =
            ManifestDigest::from_sha256_bytes(&sha2::Sha256::digest(generation.as_bytes()))
                .expect("prior generation digest");
        let prior = format!(
            r#"{{"state_digest":"{}","generation":{generation}}}"#,
            digest.as_str()
        );
        let error =
            CodeIndexPublishedGenerationV1::decode_partitioned_sealed(prior.as_bytes(), |_, _| {
                panic!("a retired manifest must be refused before any segment read")
            })
            .expect_err("prior generation must require a rebuild");
        assert!(
            matches!(
                error,
                CodeIndexProductionErrorV1::SupersededSealedGenerationRevision(refused)
                    if refused == revision
            ),
            "revision {revision} reached the wrong rejection: {error}"
        );
        assert!(error.to_string().contains("will be rebuilt from source"));
    }
}

/// Seal `generation` partitioned, keeping every segment in memory with the
/// evidence pages assembled under their pack digest.
fn partitioned_seal(
    generation: &CodeIndexPublishedGenerationV1,
) -> (Vec<u8>, std::collections::BTreeMap<String, Vec<u8>>) {
    let mut segments = std::collections::BTreeMap::new();
    let mut evidence_pack = Vec::new();
    let manifest = generation
        .encode_partitioned_sealed(|publication| {
            match publication {
                SealedGenerationSegmentPublicationV1::File { digest, bytes } => {
                    segments.insert(digest.as_str().to_owned(), bytes.to_vec());
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

fn partitioned_restore(
    manifest: &[u8],
    segments: &std::collections::BTreeMap<String, Vec<u8>>,
) -> CodeIndexPublishedGenerationV1 {
    CodeIndexPublishedGenerationV1::decode_partitioned_sealed(manifest, |request, buffer| {
        let (digest, offset, length) = match request {
            SealedGenerationSegmentReadV1::Whole { digest, size_bytes } => (digest, 0, size_bytes),
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
    })
    .expect("generation restores")
}

#[test]
fn parallel_collection_returns_the_lowest_index_failure() {
    let visited = AtomicUsize::new(0);
    let items = (0..256_usize).collect::<Vec<_>>();

    let error = collect_bounded_ordered(&items, |item, _worker| {
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
    let outcome = collect_bounded_ordered(&items, |item, _worker| {
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
    let error = collect_bounded_ordered(&items, |item, _worker| {
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
