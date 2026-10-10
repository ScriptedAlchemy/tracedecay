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

pub(super) type WorkerPublicationStore = MemorySealedPublicationStoreV1;

pub(super) struct WorkerProjectionSink;

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
            .chain(&request.changes.deleted)
            .map(|change| ChunkProjectionDecisionV1 {
                chunk_id: change.chunk_id.clone(),
                prior_chunk_digest: change.prior_digest.clone(),
                current_chunk_digest: change.current_digest.clone(),
                operation: match (&change.prior_digest, &change.current_digest) {
                    (_, None) => ProjectionOperationV1::Deleted,
                    (None, Some(_)) => ProjectionOperationV1::Added,
                    (Some(_), Some(_)) => ProjectionOperationV1::Updated,
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
    let (manifest, segments) = partitioned_seal(published.decoded().expect("cold seal"));
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

/// An unchanged tree over its sealed parent seals over it: no file is
/// extracted again and every segment and graph page carries.
#[test]
fn unchanged_successor_carries_every_parent_segment() {
    let store = WorkerPublicationStore::default();
    let mut owner =
        CodeIndexProductionOwnerV1::new(worker_config(), store.clone(), WorkerProjectionSink)
            .expect("production owner");
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
        publisher
            .publish_atomically(
                &scope,
                Some(&incumbent),
                &CodeIndexSealedPublicationV1::Cold(
                    Arc::new(active),
                    CodeIndexColdBuildReasonV1::NoParent,
                ),
            )
            .expect("historical generation seals");
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
        rebuilt.cold_reason(),
        Some(CodeIndexColdBuildReasonV1::NoParent)
    );
    let rebuilt = rebuilt.decoded().expect("cold build");
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
    let mut stale = generation.decoded().expect("cold build").files[0]
        .as_ref()
        .clone();
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
    let rebuilt = rebuilt.decoded().expect("cold build");

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
    let mut owner = CodeIndexProductionOwnerV1::new(
        worker_config(),
        WorkerPublicationStore::default(),
        WorkerProjectionSink,
    )
    .unwrap();
    let published = owner.build_and_publish(
        worker_request_with_source(
            "file.worker.attribution", 1_100_000,
            b"fn target() {}\n#[test] fn first() { target(); }\n#[test] fn second() { target(); }\n",
        ),
        &UninterruptibleCodeIndexControlV1,
    ).unwrap();
    let generation = published.decoded().unwrap();
    let cloned_generation = (**generation).clone();
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
    let mut owner = CodeIndexProductionOwnerV1::new(
        worker_config(),
        WorkerPublicationStore::default(),
        WorkerProjectionSink,
    )
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
    let mut generation = (**published.decoded().unwrap()).clone();
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
    let mut owner = CodeIndexProductionOwnerV1::new(
        worker_config(),
        WorkerPublicationStore::default(),
        WorkerProjectionSink,
    )
    .unwrap();
    let published = owner.build_and_publish(worker_request_with_source(
        "file.worker.attribution-cancel", 1_100_000,
        b"fn target() {}\n#[test] fn first() { target(); }\n#[test] fn second() { first(); }\n",
    ), &UninterruptibleCodeIndexControlV1).unwrap();
    let generation = published.decoded().unwrap();
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
