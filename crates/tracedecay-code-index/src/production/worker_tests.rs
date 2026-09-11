use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

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
        "extractor.rust.v5"
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
        "extractor.rust.v5"
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
fn upgraded_weak_lookup_releases_the_mutex_before_downstream_work() {
    let owner = Arc::new(7_u8);
    let state = Mutex::new(Some(Arc::downgrade(&owner)));

    let value = upgrade_weak_under_lock(&state, |value| value.clone()).expect("pooled value");

    assert!(
        Arc::ptr_eq(&owner, &value),
        "weak cache lookup must recover the generation's exact allocation"
    );
    let unlocked = state
        .try_lock()
        .expect("pooled lookup must not retain the mutex");
    assert_eq!(*value, 7);
    let retained = unlocked.as_ref().and_then(Weak::upgrade);
    assert_eq!(retained.as_deref(), Some(&7));
}

#[test]
fn prior_sealed_generation_is_rejected_before_manifest_decode() {
    let prior = br#"{"generation":{"format_revision":4}}"#;

    assert!(
        !CodeIndexPublishedGenerationV1::sealed_format_is_compatible(prior)
            .expect("prior format probe")
    );
    let error = CodeIndexPublishedGenerationV1::decode_sealed_if_compatible(prior)
        .expect_err("a caller that accepts incompatible durable state must not materialize it");
    assert!(error.to_string().contains("will be rebuilt from source"));
    let error = CodeIndexPublishedGenerationV1::decode_sealed(prior)
        .expect_err("prior generation must require a rebuild");
    assert!(error.to_string().contains("will be rebuilt from source"));
}

#[test]
fn parallel_collection_preserves_input_order() {
    let items = (0..1_024_usize).collect::<Vec<_>>();

    let values =
        collect_bounded_ordered(&items, |item, _worker| Ok::<_, WorkerTestError>(*item * 2))
            .expect("infallible mapping");

    assert_eq!(values.len(), items.len());
    assert!(
        values
            .iter()
            .enumerate()
            .all(|(index, value)| *value == index * 2),
        "completion order must not reorder results"
    );
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

#[test]
fn parallel_and_sequential_collection_agree() {
    let items = (0..2_048_usize).collect::<Vec<_>>();
    let sequential_operation =
        |item: &usize| Ok::<_, WorkerTestError>(item.wrapping_mul(2_654_435_761));
    let parallel_operation = |item: &usize, _worker: &crate::hotpath_observe::WorkerBusyGuard| {
        Ok::<_, WorkerTestError>(item.wrapping_mul(2_654_435_761))
    };

    let sequential = items
        .iter()
        .map(sequential_operation)
        .collect::<Result<Vec<_>, WorkerTestError>>();
    let parallel = collect_bounded_ordered(&items, parallel_operation);

    assert_eq!(sequential, parallel);
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
