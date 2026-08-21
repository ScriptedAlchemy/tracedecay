use std::{
    cell::Cell,
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex},
    time::Duration,
};

use tracedecay_code_extraction::incremental::ParseLimits;
use tracedecay_code_index::{
    chunks::{CodeIndexImportEvidenceV1, ExtractionAdmittedCodeSearchChunkV1, content_digest},
    graph_projection::{
        CODE_GRAPH_PROJECTOR_REVISION, CodeGraphProjectionError,
        build_published_code_graph_manifest_checked, code_graph_projection_identity,
    },
    production::{
        CodeIndexAtomicPublicationPort, CodeIndexBuildRequestV1, CodeIndexCapturedFileV1,
        CodeIndexExecutionControlV1, CodeIndexGenerationScopeV1, CodeIndexInterruptionV1,
        CodeIndexProductionConfigV1, CodeIndexProductionErrorV1, CodeIndexProductionOwnerV1,
        CodeIndexPublicationStoreErrorV1, CodeIndexPublishedGenerationV1,
        CodeIndexRepositoryParseIdentityV1, SEALED_GENERATION_FORMAT_REVISION_V1,
        VerifiedSealedLexicalPageReadV1, VerifiedSealedLexicalPageSourceV1,
        sealed_generation_payload_digest,
    },
    projection::{
        ChunkProjectionDecisionV1, CodeChunkProjectionSink, ProjectionReceiptBuilderV1,
        ProjectionSinkErrorV1, ProjectionSinkReceiptV1,
    },
    provider::GenerationTestAttributionJoinReadPort,
    retained_parse::{RetainedParsePoolLimits, SharedRetainedParsePool},
};
use tracedecay_domain::{
    BranchStackNodeV1, ChunkerRevision, CodeGenerationId, CommitId, FileOccurrenceId, LanguageId,
    ManifestDigest, PolicyRevisionId, PrivacyDomainId, ProjectId, ProjectionBatchRequestV1,
    ProjectionKeyV1, ProjectionKindV1, ProjectionOperationV1, ProjectionOutcomeV1,
    ProviderEvaluationStateV1, RefId, RepositoryDirtyStateV1, RepositoryId, SanitizationReceiptId,
    SanitizedCodeFileV1, SanitizedCodeSnapshotV1, SanitizerRevision, SnapshotFileDispositionV1,
    StackNodeId, TestAttributionEvidenceClassV1, TreeId, UtcMicros, WorktreeId,
};
use tracedecay_graph_db::{GraphDbError, GraphNamespace, GraphProjectorRevision};

use crate::support::{RUST_SOURCE, id};

mod parallel_equivalence;

#[derive(Clone, Default)]
pub(super) struct SharedPublicationStore {
    active: Arc<Mutex<BTreeMap<CodeIndexGenerationScopeV1, Arc<CodeIndexPublishedGenerationV1>>>>,
}

impl SharedPublicationStore {
    fn scope_count(&self) -> usize {
        self.active.lock().expect("publication lock").len()
    }

    fn shares_repository_store(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.active, &other.active)
    }
}

impl CodeIndexAtomicPublicationPort for SharedPublicationStore {
    fn load_active(
        &self,
        scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        Ok(self
            .active
            .lock()
            .expect("publication lock")
            .get(scope)
            .map(|generation| generation.as_ref().clone()))
    }

    fn publish_atomically(
        &mut self,
        scope: &CodeIndexGenerationScopeV1,
        expected_active_generation: Option<&CodeGenerationId>,
        generation: Arc<CodeIndexPublishedGenerationV1>,
    ) -> Result<(), CodeIndexPublicationStoreErrorV1> {
        let mut active = self.active.lock().expect("publication lock");
        if active
            .get(scope)
            .map(|current| current.manifest().generation_id.clone())
            .as_ref()
            != expected_active_generation
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        active.insert(scope.clone(), generation);
        Ok(())
    }
}

/// Models the mispartitioned publication stores the production owner must
/// refuse: one physical active pointer that ignores the requested scope, so
/// the generation sealed for one branch/worktree is answered for every scope.
#[derive(Clone, Default)]
struct PartialKeyPublicationStore {
    active: Arc<Mutex<Option<Arc<CodeIndexPublishedGenerationV1>>>>,
}

impl CodeIndexAtomicPublicationPort for PartialKeyPublicationStore {
    fn load_active(
        &self,
        _scope: &CodeIndexGenerationScopeV1,
    ) -> Result<Option<CodeIndexPublishedGenerationV1>, CodeIndexPublicationStoreErrorV1> {
        Ok(self
            .active
            .lock()
            .expect("publication lock")
            .as_ref()
            .map(|generation| generation.as_ref().clone()))
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
            .map(|current| current.manifest().generation_id.clone())
            .as_ref()
            != expected_active_generation
        {
            return Err(CodeIndexPublicationStoreErrorV1::CompareAndSwap);
        }
        *active = Some(generation);
        Ok(())
    }
}

#[derive(Default)]
pub(super) struct ApplyingProjectionSink;

impl CodeChunkProjectionSink for ApplyingProjectionSink {
    fn project_changed_chunks(
        &mut self,
        request: &ProjectionBatchRequestV1,
        receipt_builder: ProjectionReceiptBuilderV1<'_>,
    ) -> Result<ProjectionSinkReceiptV1, ProjectionSinkErrorV1> {
        let mut decisions: Vec<ChunkProjectionDecisionV1> = request
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
                output_digest: Some(
                    change
                        .current_digest
                        .clone()
                        .expect("added or changed chunks have a current digest"),
                ),
            })
            .collect();
        decisions.extend(
            request
                .changes
                .deleted
                .iter()
                .map(|change| ChunkProjectionDecisionV1 {
                    chunk_id: change.chunk_id.clone(),
                    prior_chunk_digest: change.prior_digest.clone(),
                    current_chunk_digest: None,
                    operation: ProjectionOperationV1::Deleted,
                    outcome: ProjectionOutcomeV1::Applied,
                    output_digest: None,
                }),
        );
        decisions.extend(
            request
                .changes
                .reused
                .iter()
                .map(|change| ChunkProjectionDecisionV1 {
                    chunk_id: change.chunk_id.clone(),
                    prior_chunk_digest: change.prior_digest.clone(),
                    current_chunk_digest: change.current_digest.clone(),
                    operation: ProjectionOperationV1::Reused,
                    outcome: ProjectionOutcomeV1::Reused,
                    output_digest: None,
                }),
        );
        receipt_builder
            .build(&decisions)
            .map_err(|error| ProjectionSinkErrorV1::Rejected(error.to_string()))
    }
}

struct RejectingProjectionSink;

impl CodeChunkProjectionSink for RejectingProjectionSink {
    fn project_changed_chunks(
        &mut self,
        _request: &ProjectionBatchRequestV1,
        _receipt_builder: ProjectionReceiptBuilderV1<'_>,
    ) -> Result<ProjectionSinkReceiptV1, ProjectionSinkErrorV1> {
        Err(ProjectionSinkErrorV1::Rejected(
            "projection is intentionally unavailable".to_owned(),
        ))
    }
}

pub(super) struct ActiveControl;

impl CodeIndexExecutionControlV1 for ActiveControl {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

struct CancelledControl;

impl CodeIndexExecutionControlV1 for CancelledControl {
    fn is_cancelled(&self) -> bool {
        true
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

struct ExpiredControl;

impl CodeIndexExecutionControlV1 for ExpiredControl {
    fn is_cancelled(&self) -> bool {
        false
    }

    fn is_deadline_exceeded(&self) -> bool {
        true
    }
}

#[derive(Default)]
struct MutableCancellationControl {
    cancelled: AtomicBool,
}

impl MutableCancellationControl {
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    fn resume(&self) {
        self.cancelled.store(false, Ordering::Release);
    }
}

impl CodeIndexExecutionControlV1 for MutableCancellationControl {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn is_deadline_exceeded(&self) -> bool {
        false
    }
}

pub(super) fn config() -> CodeIndexProductionConfigV1 {
    CodeIndexProductionConfigV1 {
        project_id: id::<ProjectId>("project.production"),
        repository: id::<RepositoryId>("repository.production"),
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
        policy_revision: id::<PolicyRevisionId>("policy.v1"),
        chunker_revision: id::<ChunkerRevision>("chunker.v2"),
        privacy_domain: id::<PrivacyDomainId>("privacy.production"),
        privacy_key_epoch: 7,
        max_snapshot_age_micros: None,
    }
}

fn projection_key() -> ProjectionKeyV1 {
    ProjectionKeyV1 {
        kind: ProjectionKindV1::Lexical,
        schema_revision: "lexical.v1".to_owned(),
        profile_digest: id::<ManifestDigest>(&format!("sha256:{}", "e".repeat(64))),
    }
}

fn request(file_occurrence: &str, sealed_at: i64) -> CodeIndexBuildRequestV1 {
    request_at_path(file_occurrence, "src/lib.rs", sealed_at)
}

fn request_in_scope(
    file_occurrence: &str,
    sealed_at: i64,
    reference: &str,
    worktree: Option<&str>,
    source_revision: &str,
) -> CodeIndexBuildRequestV1 {
    let mut request = request(file_occurrence, sealed_at);
    request.snapshot.reference = Some(id::<RefId>(reference));
    request.snapshot.worktree = worktree.map(id::<WorktreeId>);
    request.snapshot.source_revision = Some(id::<CommitId>(source_revision));
    request.repository_parse_identity = CodeIndexRepositoryParseIdentityV1 {
        tree: Some(id::<TreeId>(&format!("tree.{source_revision}"))),
        dirty: RepositoryDirtyStateV1::Dirty,
    };
    request
}

fn request_at_path(
    file_occurrence: &str,
    logical_path: &str,
    sealed_at: i64,
) -> CodeIndexBuildRequestV1 {
    let source = RUST_SOURCE.as_bytes();
    let file = SanitizedCodeFileV1 {
        file_occurrence_id: id::<FileOccurrenceId>(file_occurrence),
        logical_path: logical_path.to_owned(),
        language: Some(id::<LanguageId>("rust")),
        content_digest: content_digest(source),
        disposition: SnapshotFileDispositionV1::Present,
    };
    let snapshot = SanitizedCodeSnapshotV1 {
        repository: id::<RepositoryId>("repository.production"),
        worktree: None,
        reference: None,
        source_revision: None,
        sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
        sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.production")],
        content_identity: content_digest(source),
        captured_at: UtcMicros(1_000_000),
        files: vec![file.clone()],
    };

    CodeIndexBuildRequestV1 {
        snapshot,
        captured_files: vec![CodeIndexCapturedFileV1 {
            file_occurrence_id: file.file_occurrence_id,
            sanitized_bytes: source.to_vec(),
            sensitivity_level: tracedecay_domain::SensitivityLevelV1::Public,
        }],
        changed_files: BTreeSet::new(),
        invalidations: BTreeSet::new(),
        ignored_source_admissions: Vec::new(),
        repository_parse_identity: CodeIndexRepositoryParseIdentityV1 {
            tree: None,
            dirty: RepositoryDirtyStateV1::Dirty,
        },
        sealed_at: UtcMicros(sealed_at),
        target_projection_key: projection_key(),
    }
}

pub(super) fn request_with_source(
    file_occurrence: &str,
    sealed_at: i64,
    source_revision: &str,
    tree: &str,
    source: &str,
) -> CodeIndexBuildRequestV1 {
    let mut request = request_in_scope(
        file_occurrence,
        sealed_at,
        "refs/heads/feature",
        Some("worktree.feature"),
        source_revision,
    );
    let bytes = source.as_bytes().to_vec();
    request.snapshot.files[0].content_digest = content_digest(&bytes);
    request.snapshot.content_identity = content_digest(&bytes);
    request.captured_files[0].sanitized_bytes = bytes;
    request.repository_parse_identity.tree = Some(id::<TreeId>(tree));
    request.changed_files.insert("src/lib.rs".to_owned());
    request
}

#[test]
fn production_increment_reuses_retained_tree_and_reports_bounded_parse_work() {
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
        .expect("production owner");
    owner
        .build_and_publish(
            request_with_source(
                "file.retained.1",
                1_100_000,
                "commit.retained.1",
                "tree.retained.1",
                "fn unchanged() -> u32 { 1 }\nfn edited() -> u32 { 2 }\n",
            ),
            &ActiveControl,
        )
        .expect("initial generation");
    owner
        .build_and_publish(
            request_with_source(
                "file.retained.2",
                1_200_000,
                "commit.retained.2",
                "tree.retained.2",
                "fn unchanged() -> u32 { 1 }\nfn edited() -> u32 { 20 }\n",
            ),
            &ActiveControl,
        )
        .expect("incremental generation");

    let stats = owner.retained_parse_stats();
    assert_eq!(stats.initial_parses, 1);
    assert_eq!(stats.incremental_parses, 1);
    assert_eq!(stats.full_extractions, 1);
    assert_eq!(stats.incremental_extractions, 1);
    assert_eq!(stats.reset_extractions, 0);
    assert_eq!(stats.retained_documents, 1);
    assert!(stats.changed_bytes < 60);
    assert!(stats.visited_top_level_nodes <= 3);
    assert!(stats.extracted_bytes < 120);
}

/// One file exceeding the bounded per-file parse budget must never fail the
/// whole build: the generation still completes, publishes, and serves, with
/// the slow file recorded as a typed unsupported document (with a reason) and
/// truthful coverage accounting.
#[test]
fn slow_parse_file_publishes_a_completed_generation_with_a_typed_omission() {
    // The retained parser's deadline is only observed every ~100 Tree-sitter
    // parse operations, so the tiny file completes before the first progress
    // check while the generated file reliably crosses many of them. A 1ns
    // budget therefore deterministically times out exactly the large file.
    let pool = SharedRetainedParsePool::new(RetainedParsePoolLimits {
        document: ParseLimits {
            max_parse_time: Duration::from_nanos(1),
            ..ParseLimits::default()
        },
        ..RetainedParsePoolLimits::default()
    })
    .expect("retained parse pool");
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
        .expect("production owner")
        .with_retained_parse_pool(pool);

    let fast_source = "fn fast() -> u32 { 1 }\n";
    let slow_source = (0..2_000)
        .map(|index| format!("fn generated_{index}() -> u64 {{ {index} }}\n"))
        .collect::<String>();
    let fast = SanitizedCodeFileV1 {
        file_occurrence_id: id::<FileOccurrenceId>("file.fast"),
        logical_path: "src/fast.rs".to_owned(),
        language: Some(id::<LanguageId>("rust")),
        content_digest: content_digest(fast_source.as_bytes()),
        disposition: SnapshotFileDispositionV1::Present,
    };
    let slow = SanitizedCodeFileV1 {
        file_occurrence_id: id::<FileOccurrenceId>("file.slow"),
        logical_path: "src/slow.rs".to_owned(),
        language: Some(id::<LanguageId>("rust")),
        content_digest: content_digest(slow_source.as_bytes()),
        disposition: SnapshotFileDispositionV1::Present,
    };
    let request = CodeIndexBuildRequestV1 {
        snapshot: SanitizedCodeSnapshotV1 {
            repository: id::<RepositoryId>("repository.production"),
            worktree: None,
            reference: None,
            source_revision: None,
            sanitizer_revision: id::<SanitizerRevision>("sanitizer.v1"),
            sanitization_receipts: vec![id::<SanitizationReceiptId>("receipt.production")],
            content_identity: content_digest(format!("{fast_source}{slow_source}").as_bytes()),
            captured_at: UtcMicros(1_000_000),
            files: vec![fast.clone(), slow.clone()],
        },
        captured_files: vec![
            CodeIndexCapturedFileV1 {
                file_occurrence_id: fast.file_occurrence_id.clone(),
                sanitized_bytes: fast_source.as_bytes().to_vec(),
                sensitivity_level: tracedecay_domain::SensitivityLevelV1::Public,
            },
            CodeIndexCapturedFileV1 {
                file_occurrence_id: slow.file_occurrence_id.clone(),
                sanitized_bytes: slow_source.into_bytes(),
                sensitivity_level: tracedecay_domain::SensitivityLevelV1::Public,
            },
        ],
        changed_files: BTreeSet::new(),
        invalidations: BTreeSet::new(),
        ignored_source_admissions: Vec::new(),
        repository_parse_identity: CodeIndexRepositoryParseIdentityV1 {
            tree: None,
            dirty: RepositoryDirtyStateV1::Dirty,
        },
        sealed_at: UtcMicros(1_100_000),
        target_projection_key: projection_key(),
    };

    let generation = owner
        .build_and_publish(request, &ActiveControl)
        .expect("a slow-parse file must not fail the whole generation");

    // Truthful coverage: both files eligible, exactly the slow one omitted.
    assert_eq!(generation.coverage().files_eligible, 2);
    assert_eq!(generation.coverage().files_unsupported, 1);

    // The generation serves: the fast file's chunks are admitted, and no
    // chunk was invented for the timed-out file.
    let admitted = generation
        .admitted_chunks()
        .expect("published generation admits exact chunks");
    assert!(!admitted.is_empty());
    assert!(
        admitted
            .iter()
            .all(|chunk| chunk.chunk().anchor.file_occurrence_id.as_str() == "file.fast")
    );

    // The omission is a typed per-file document state with a reason, durable
    // through sealing.
    let sealed = generation.encode_sealed().expect("generation seals");
    let value: serde_json::Value = serde_json::from_slice(&sealed).expect("sealed JSON");
    let slow_document = value["generation"]["files"]
        .as_array()
        .expect("sealed files")
        .iter()
        .map(|file| &file["artifacts"]["chunks"]["document"])
        .find(|document| document["file_occurrence_id"] == "file.slow")
        .expect("slow file document is retained in the generation");
    assert_eq!(slow_document["eligibility"]["eligibility"], "unsupported");
    let reason = slow_document["eligibility"]["reason"]["reason"]
        .as_str()
        .expect("typed omission carries a reason");
    assert!(
        reason.contains("parse budget"),
        "unexpected omission reason: {reason}"
    );
}

#[test]
fn retained_parse_syntax_errors_publish_partial_generation_coverage() {
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
        .expect("production owner");
    let generation = owner
        .build_and_publish(
            request_with_source(
                "file.retained.partial",
                1_100_000,
                "commit.retained.partial",
                "tree.retained.partial",
                "fn broken(\n",
            ),
            &ActiveControl,
        )
        .expect("partial generation publishes");

    assert_eq!(generation.coverage().files_partial, 1);
}

#[test]
fn published_generation_serves_current_conservative_test_attribution() {
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
        .expect("production owner");
    let generation = owner
        .build_and_publish(
            request_at_path("file.production.test", "tests/production.rs", 1_100_000),
            &ActiveControl,
        )
        .expect("test generation publishes");
    let authority = generation
        .test_attribution_authority()
        .expect("attribution authority");

    let read = authority.read_test_attribution(&generation.manifest().generation_id);

    assert!(matches!(
        read.provider_state,
        ProviderEvaluationStateV1::SupportedCompletedComplete | ProviderEvaluationStateV1::Partial
    ));
    let join = read.evidence.expect("generation attribution");
    assert_eq!(join.generation_id, generation.manifest().generation_id);
    assert_eq!(
        join.test_watermark.snapshot_digest,
        generation.manifest().snapshot_digest
    );
    assert!(!join.records.is_empty());
    assert!(join.records.iter().all(|record| {
        record.attribution.evidence_class
            == TestAttributionEvidenceClassV1::ConservativeDependencyCandidates
    }));
}

#[test]
fn production_owner_publishes_complete_generation_and_restores_it_after_restart() {
    let store = SharedPublicationStore::default();
    let mut owner =
        CodeIndexProductionOwnerV1::new(config(), store.clone(), ApplyingProjectionSink)
            .expect("production owner");

    let first = owner
        .build_and_publish(request("file.production.1", 1_100_000), &ActiveControl)
        .expect("first generation publishes");

    assert_eq!(first.coverage().files_eligible, 1);
    assert!(first.edges().len() + first.edge_abstentions().len() > 0);
    assert!(
        !first
            .admitted_chunks()
            .expect("parser-backed exact authority")
            .is_empty()
    );
    assert_eq!(
        first.projection().receipt().source_generation,
        first.manifest().generation_id
    );

    let mut restarted =
        CodeIndexProductionOwnerV1::new(config(), store.clone(), ApplyingProjectionSink)
            .expect("restart owner");
    let restored = restarted
        .active_generation(&CodeIndexGenerationScopeV1::for_snapshot(
            &request("file.production.scope", 1_100_000).snapshot,
        ))
        .expect("active generation loads")
        .expect("published generation survives restart");
    assert_eq!(restored.manifest(), first.manifest());

    let second = restarted
        .build_and_publish(request("file.production.2", 1_200_000), &ActiveControl)
        .expect("unchanged source carries forward");
    assert_eq!(
        second.manifest().parent_generation,
        Some(first.manifest().generation_id.clone())
    );
    assert!(
        second
            .projection()
            .request()
            .changes
            .added_or_changed
            .is_empty()
    );
    assert!(second.projection().request().changes.deleted.is_empty());
    assert!(!second.projection().request().changes.reused.is_empty());
    assert!(
        !second
            .admitted_chunks()
            .expect("carry-forward retains parser-backed exact authority")
            .is_empty()
    );
}

#[test]
fn published_graph_manifest_projects_files_chunks_symbols_and_replays_byte_identically() {
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
        .expect("production owner");
    let generation = owner
        .build_and_publish(request("file.graph-projection", 1_250_000), &ActiveControl)
        .expect("generation publishes");
    let projector_revision =
        GraphProjectorRevision::try_from(CODE_GRAPH_PROJECTOR_REVISION.to_owned())
            .expect("projector revision");
    let projection =
        code_graph_projection_identity(GraphNamespace::new("code-graph-test").expect("namespace"))
            .expect("projection identity");
    let manifest = build_published_code_graph_manifest_checked(
        projection.clone(),
        &generation,
        &projector_revision,
        &|| Ok(()),
    )
    .expect("published generation projects");

    let label_count = |label: &str| {
        manifest
            .entities
            .iter()
            .filter(|entity| {
                entity
                    .labels
                    .iter()
                    .any(|candidate| candidate.as_str() == label)
            })
            .count()
    };
    assert_eq!(label_count("CodeFile"), generation.snapshot().files.len());
    assert_eq!(label_count("CodeChunk"), generation.chunks().chunks().len());
    assert_eq!(
        label_count("CodeSymbol"),
        generation.symbols().symbols.len()
    );
    assert!(
        manifest
            .relations
            .iter()
            .any(|relation| { relation.kind.as_str() == "CodeFileContainsSymbol" })
    );
    assert!(
        manifest
            .relations
            .iter()
            .any(|relation| { relation.kind.as_str() == "CodeChunkDescribesSymbol" })
    );

    let sealed = generation.encode_sealed().expect("generation seals");
    let restored =
        CodeIndexPublishedGenerationV1::decode_sealed(&sealed).expect("generation restores");
    let replayed = build_published_code_graph_manifest_checked(
        projection,
        &restored,
        &projector_revision,
        &|| Ok(()),
    )
    .expect("restored generation projects");
    assert_eq!(
        manifest
            .expected_recovered_digest(&|| Ok(()))
            .expect("original projection digest"),
        replayed
            .expected_recovered_digest(&|| Ok(()))
            .expect("replayed projection digest")
    );
}

/// The graph publication manifest is a pure function of the immutable
/// generation, so seat retries and the seat/reconcile duplicate publication of
/// one sealed generation must not re-examine every chunk, symbol, and edge.
/// The memo is fail-closed: a deadline mid-build records nothing, a memo hit
/// still refuses an expired request, and a foreign projection identity or
/// projector revision rebuilds in full instead of aliasing the cached
/// manifest.
#[test]
fn repeated_graph_manifest_builds_reuse_the_memo_without_reexamining_the_generation() {
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
        .expect("production owner");
    let generation = owner
        .build_and_publish(request("file.graph-memo", 1_260_000), &ActiveControl)
        .expect("generation publishes");
    let projector_revision =
        GraphProjectorRevision::try_from(CODE_GRAPH_PROJECTOR_REVISION.to_owned())
            .expect("projector revision");
    let projection =
        code_graph_projection_identity(GraphNamespace::new("code-graph-memo").expect("namespace"))
            .expect("projection identity");

    // A deadline mid-build is a failed generation build that memoizes nothing.
    let interrupted_checks = Cell::new(0usize);
    let interrupted = build_published_code_graph_manifest_checked(
        projection.clone(),
        &generation,
        &projector_revision,
        &|| {
            interrupted_checks.set(interrupted_checks.get() + 1);
            if interrupted_checks.get() > 3 {
                Err(GraphDbError::DeadlineExceeded)
            } else {
                Ok(())
            }
        },
    )
    .expect_err("a deadline mid-build fails the build");
    assert_eq!(interrupted, CodeGraphProjectionError::DeadlineExceeded);

    let first_checks = Cell::new(0usize);
    let first = build_published_code_graph_manifest_checked(
        projection.clone(),
        &generation,
        &projector_revision,
        &|| {
            first_checks.set(first_checks.get() + 1);
            Ok(())
        },
    )
    .expect("first complete build");
    let second_checks = Cell::new(0usize);
    let second = build_published_code_graph_manifest_checked(
        projection.clone(),
        &generation,
        &projector_revision,
        &|| {
            second_checks.set(second_checks.get() + 1);
            Ok(())
        },
    )
    .expect("memoized build");
    let first_weak = Arc::downgrade(&first);
    assert!(
        Arc::ptr_eq(&first, &second),
        "a live memo hit must return the exact manifest allocation"
    );
    assert_eq!(first, second, "the memo returns the identical manifest");
    assert!(
        !first.entities.is_empty(),
        "fixture must publish graph entities"
    );
    assert!(
        !first.relations.is_empty(),
        "fixture must publish graph relations"
    );
    assert_eq!(
        first.entities.as_ptr(),
        second.entities.as_ptr(),
        "a memo hit must share the immutable entity buffer instead of deep-cloning it"
    );
    assert_eq!(
        first.relations.as_ptr(),
        second.relations.as_ptr(),
        "a memo hit must share the immutable relation buffer instead of deep-cloning it"
    );
    assert!(
        first_checks.get() > 3,
        "the interrupted build must not have been memoized (first build saw {} checks)",
        first_checks.get()
    );
    assert!(
        first_checks.get() > first.entities.len() / 4,
        "a fresh build examines the generation item by item ({} checks over {} entities)",
        first_checks.get(),
        first.entities.len()
    );
    assert_eq!(
        second_checks.get(),
        1,
        "a memo hit performs the admission check only, with no per-item examination"
    );

    // A memo hit still refuses an already-expired request.
    let refused = build_published_code_graph_manifest_checked(
        projection.clone(),
        &generation,
        &projector_revision,
        &|| Err(GraphDbError::DeadlineExceeded),
    )
    .expect_err("an expired request is refused before the memo serves");
    assert_eq!(refused, CodeGraphProjectionError::DeadlineExceeded);

    drop(first);
    drop(second);
    assert!(
        first_weak.upgrade().is_none(),
        "the generation must not pin a graph manifest after its callers release it"
    );

    let rebuilt_checks = Cell::new(0usize);
    let rebuilt_same_key = build_published_code_graph_manifest_checked(
        projection.clone(),
        &generation,
        &projector_revision,
        &|| {
            rebuilt_checks.set(rebuilt_checks.get() + 1);
            Ok(())
        },
    )
    .expect("an expired same-key memo rebuilds");
    assert!(
        rebuilt_checks.get() > 3,
        "an expired weak memo must rebuild the manifest"
    );
    let refreshed_checks = Cell::new(0usize);
    let refreshed = build_published_code_graph_manifest_checked(
        projection.clone(),
        &generation,
        &projector_revision,
        &|| {
            refreshed_checks.set(refreshed_checks.get() + 1);
            Ok(())
        },
    )
    .expect("the rebuilt manifest refreshes the memo");
    assert!(Arc::ptr_eq(&rebuilt_same_key, &refreshed));
    assert_eq!(
        refreshed_checks.get(),
        1,
        "a live refreshed memo performs only the admission check"
    );

    // A foreign projection identity is a memo miss that rebuilds in full.
    let foreign = code_graph_projection_identity(
        GraphNamespace::new("code-graph-memo-other").expect("namespace"),
    )
    .expect("projection identity");
    let foreign_checks = Cell::new(0usize);
    let rebuilt = build_published_code_graph_manifest_checked(
        foreign.clone(),
        &generation,
        &projector_revision,
        &|| {
            foreign_checks.set(foreign_checks.get() + 1);
            Ok(())
        },
    )
    .expect("foreign projection rebuilds");
    assert_eq!(rebuilt.projection, foreign);
    assert!(
        foreign_checks.get() > 1,
        "a foreign projection identity cannot serve the cached manifest"
    );
}

#[test]
fn sealed_generation_validation_is_memoized_but_decode_stays_fail_closed() {
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
        .expect("production owner");
    let generation = owner
        .build_and_publish(request("file.project-binding", 1_200_000), &ActiveControl)
        .expect("valid generation publishes");
    let sealed = generation.encode_sealed().expect("valid generation seals");
    assert_eq!(
        generation
            .encode_sealed()
            .expect("memoized generation seals again"),
        sealed
    );

    let restored =
        CodeIndexPublishedGenerationV1::decode_sealed(&sealed).expect("valid generation restores");
    assert_eq!(restored.manifest().project_id, config().project_id);
    assert_eq!(
        restored
            .encode_sealed()
            .expect("restored generation retains successful validation"),
        sealed
    );

    let mut envelope: serde_json::Value =
        serde_json::from_slice(&sealed).expect("sealed generation JSON");
    envelope["generation"]["files"][0]["authority"]["project_id"] =
        serde_json::Value::String("project.foreign".to_owned());
    let state_digest = sealed_generation_payload_digest(
        SEALED_GENERATION_FORMAT_REVISION_V1,
        &envelope["generation"],
    )
    .expect("forged payload has a state digest");
    envelope["state_digest"] = serde_json::Value::String(state_digest.as_str().to_owned());
    let forged = serde_json::to_vec(&envelope).expect("forged sealed generation JSON");

    let error = CodeIndexPublishedGenerationV1::decode_sealed(&forged)
        .expect_err("foreign file authority must fail sealed restoration");
    assert!(
        error
            .to_string()
            .contains("file authority project does not match the generation manifest"),
        "unexpected project mismatch error: {error}"
    );
}

#[test]
fn verified_sealed_lexical_pages_are_bounded_exact_and_resumable_after_cancellation() {
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
        .expect("production owner");
    let generation = owner
        .build_and_publish(
            request_with_source(
                "file.lexical-pages",
                1_250_000,
                "commit.lexical-pages",
                "tree.lexical-pages",
                "fn alpha() -> u32 { 1 }\nfn beta() -> u32 { alpha() + 1 }\nfn gamma() -> u32 { beta() + 1 }\n",
            ),
            &ActiveControl,
        )
        .expect("generation publishes");
    let sealed = generation.encode_sealed().expect("generation seals");
    let envelope: serde_json::Value =
        serde_json::from_slice(&sealed).expect("sealed generation JSON");
    let expected_state_digest = id::<ManifestDigest>(
        envelope["state_digest"]
            .as_str()
            .expect("state digest string"),
    );
    let control = MutableCancellationControl::default();
    let mut source = VerifiedSealedLexicalPageSourceV1::open(
        Cursor::new(sealed.clone()),
        u64::try_from(sealed.len()).expect("sealed length"),
        expected_state_digest.clone(),
        1,
        1024 * 1024,
        &control,
    )
    .expect("verified page source opens");

    let first = match source.next_page(&control).expect("first page") {
        VerifiedSealedLexicalPageReadV1::Page(page) => page,
        VerifiedSealedLexicalPageReadV1::Complete(_) => {
            panic!("fixture must emit at least one lexical page")
        }
    };
    assert_eq!(first.page_ordinal(), 0);
    assert_eq!(first.chunk_count(), 1);
    assert_eq!(first.chunks().len(), 1);
    assert!(first.payload_bytes() > 0);
    assert!(first.payload_bytes() <= 1024 * 1024);

    control.cancel();
    let cursor_before_cancel = source.cursor().clone();
    let error = source
        .next_page(&control)
        .expect_err("cancellation must interrupt before another page is admitted");
    assert!(matches!(
        error,
        CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::Cancelled)
    ));
    assert_eq!(source.cursor(), &cursor_before_cancel);
    control.resume();

    let mut observed = first
        .chunks()
        .iter()
        .map(|chunk| chunk.chunk().clone())
        .collect::<Vec<_>>();
    let mut final_page_digest = first.cumulative_digest().clone();
    let receipt = loop {
        match source.next_page(&control).expect("resumed page read") {
            VerifiedSealedLexicalPageReadV1::Page(page) => {
                assert_eq!(page.chunk_count(), 1);
                assert_eq!(page.chunks().len(), 1);
                assert!(page.payload_bytes() <= 1024 * 1024);
                final_page_digest = page.cumulative_digest().clone();
                observed.extend(page.chunks().iter().map(|chunk| chunk.chunk().clone()));
            }
            VerifiedSealedLexicalPageReadV1::Complete(receipt) => break receipt,
        }
    };

    let mut expected = generation
        .admitted_chunks()
        .expect("published exact chunks")
        .iter()
        .map(|chunk| chunk.chunk().clone())
        .collect::<Vec<_>>();
    observed.sort_by(|left, right| left.id.cmp(&right.id));
    expected.sort_by(|left, right| left.id.cmp(&right.id));
    assert_eq!(observed, expected);
    assert_eq!(receipt.total_chunks(), expected.len() as u64);
    assert_eq!(receipt.page_count(), expected.len() as u64);
    assert_eq!(receipt.cumulative_digest(), &final_page_digest);
    assert_eq!(receipt.source_state_digest(), &expected_state_digest);
    assert_eq!(
        receipt.format_revision(),
        SEALED_GENERATION_FORMAT_REVISION_V1
    );
}

#[test]
fn verified_sealed_lexical_source_refuses_a_foreign_state_digest() {
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
        .expect("production owner");
    let generation = owner
        .build_and_publish(request("file.lexical-digest", 1_260_000), &ActiveControl)
        .expect("generation publishes");
    let sealed = generation.encode_sealed().expect("generation seals");
    let error = VerifiedSealedLexicalPageSourceV1::open(
        Cursor::new(sealed.clone()),
        u64::try_from(sealed.len()).expect("sealed length"),
        id::<ManifestDigest>(&format!("sha256:{}", "0".repeat(64))),
        16,
        1024 * 1024,
        &ActiveControl,
    )
    .expect_err("a foreign durable state digest must not authorize lexical bytes");
    assert!(
        error
            .to_string()
            .contains("state digest does not match the admitted source"),
        "unexpected digest error: {error}"
    );
}

#[test]
fn verified_sealed_lexical_imports_are_exact_once_and_page_boundary_independent() {
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
        .expect("production owner");
    let mut request = request_with_source(
        "file.lexical-imports",
        1_265_000,
        "commit.lexical-imports",
        "tree.lexical-imports",
        "import type { Widget } from \"widget-kit\";\nexport function render(value: Widget) { return value; }\n",
    );
    request.snapshot.files[0].logical_path = "src/imports.ts".to_owned();
    request.snapshot.files[0].language = Some(id::<LanguageId>("typescript"));
    request.changed_files.clear();
    request.changed_files.insert("src/imports.ts".to_owned());
    request
        .snapshot
        .validate()
        .expect("TypeScript import snapshot is canonical");
    let generation = owner
        .build_and_publish(request, &ActiveControl)
        .expect("generation publishes");
    assert!(
        !generation.imports().is_empty(),
        "the fixture must contain parser-backed import evidence"
    );
    let sealed = generation.encode_sealed().expect("generation seals");
    let envelope: serde_json::Value =
        serde_json::from_slice(&sealed).expect("sealed generation JSON");
    let expected_state_digest = id::<ManifestDigest>(
        envelope["state_digest"]
            .as_str()
            .expect("state digest string"),
    );

    let read = |maximum_page_chunks| {
        let mut source = VerifiedSealedLexicalPageSourceV1::open(
            Cursor::new(sealed.clone()),
            u64::try_from(sealed.len()).expect("sealed length"),
            expected_state_digest.clone(),
            maximum_page_chunks,
            1024 * 1024,
            &ActiveControl,
        )
        .expect("verified import page source opens");
        let mut imports = Vec::new();
        let receipt = loop {
            match source
                .next_page(&ActiveControl)
                .expect("verified import page")
            {
                VerifiedSealedLexicalPageReadV1::Page(page) => {
                    assert_eq!(page.import_count(), page.imports().len() as u64);
                    assert!(
                        page.payload_bytes() + page.import_payload_bytes() <= 1024 * 1024,
                        "chunks and imports share one page byte bound"
                    );
                    if page.import_count() > 0 {
                        assert!(page.import_payload_bytes() > 0);
                    }
                    imports.extend(page.imports().iter().cloned());
                }
                VerifiedSealedLexicalPageReadV1::Complete(receipt) => break receipt,
            }
        };
        (imports, receipt)
    };

    let (split_imports, split_receipt) = read(1);
    let (wide_imports, wide_receipt) = read(64);
    assert_eq!(split_imports, generation.imports());
    assert_eq!(wide_imports, generation.imports());
    assert_eq!(
        split_receipt.total_imports(),
        generation.imports().len() as u64
    );
    assert!(split_receipt.import_payload_bytes() > 0);
    assert_eq!(
        split_receipt.import_dictionary_digest(),
        wide_receipt.import_dictionary_digest(),
        "the exact import dictionary cannot depend on page boundaries"
    );
    assert_eq!(
        split_receipt.import_payload_bytes(),
        wide_receipt.import_payload_bytes()
    );
}

#[test]
fn verified_sealed_lexical_page_transition_is_canonical_across_importing_files() {
    let first_source = "import type { Beta } from \"./beta\";\nexport type Alpha = Beta;\n";
    let second_source = "import type { Alpha } from \"./alpha\";\nexport type Beta = Alpha;\n";
    let mut request = request_with_source(
        "file.lexical-order.alpha",
        1_266_000,
        "commit.lexical-order",
        "tree.lexical-order",
        first_source,
    );
    request.snapshot.files[0].logical_path = "src/alpha.ts".to_owned();
    request.snapshot.files[0].language = Some(id::<LanguageId>("typescript"));
    let second_file = SanitizedCodeFileV1 {
        file_occurrence_id: id::<FileOccurrenceId>("file.lexical-order.beta"),
        logical_path: "src/beta.ts".to_owned(),
        language: Some(id::<LanguageId>("typescript")),
        content_digest: content_digest(second_source.as_bytes()),
        disposition: SnapshotFileDispositionV1::Present,
    };
    request.snapshot.files.push(second_file.clone());
    request.snapshot.content_identity =
        content_digest(format!("{first_source}{second_source}").as_bytes());
    request.captured_files.push(CodeIndexCapturedFileV1 {
        file_occurrence_id: second_file.file_occurrence_id.clone(),
        sanitized_bytes: second_source.as_bytes().to_vec(),
        sensitivity_level: tracedecay_domain::SensitivityLevelV1::Public,
    });
    request.changed_files.clear();
    request.changed_files.insert("src/alpha.ts".to_owned());
    request.changed_files.insert("src/beta.ts".to_owned());
    request
        .snapshot
        .validate()
        .expect("two-file TypeScript snapshot is canonical");

    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
        .expect("production owner");
    let generation = owner
        .build_and_publish(request, &ActiveControl)
        .expect("generation publishes");
    let import_files = generation
        .imports()
        .iter()
        .map(|evidence| evidence.file_occurrence_id.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        import_files.len(),
        2,
        "both files must contribute parser-backed import evidence"
    );

    let sealed = generation.encode_sealed().expect("generation seals");
    let envelope: serde_json::Value =
        serde_json::from_slice(&sealed).expect("sealed generation JSON");
    let state_digest = id::<ManifestDigest>(
        envelope["state_digest"]
            .as_str()
            .expect("state digest string"),
    );
    let mut source = VerifiedSealedLexicalPageSourceV1::open(
        Cursor::new(sealed.clone()),
        u64::try_from(sealed.len()).expect("sealed length"),
        state_digest,
        usize::MAX,
        1024 * 1024,
        &ActiveControl,
    )
    .expect("verified page source opens");
    let mut previous_cursor = None;
    let mut observed_chunks = Vec::new();
    let mut observed_imports = Vec::new();
    let receipt = loop {
        match source.next_page(&ActiveControl).expect("verified page") {
            VerifiedSealedLexicalPageReadV1::Page(page) => {
                assert!(page.payload_bytes() + page.import_payload_bytes() <= 1024 * 1024);
                page.verify_transition(previous_cursor.as_ref())
                    .expect("a source-minted cross-file page must verify");
                previous_cursor = Some(page.next_cursor().clone());
                observed_chunks.extend(page.chunks().iter().map(|chunk| chunk.chunk().clone()));
                observed_imports.extend(page.imports().iter().cloned());
            }
            VerifiedSealedLexicalPageReadV1::Complete(receipt) => break receipt,
        }
    };
    receipt
        .verify_completion(previous_cursor.as_ref())
        .expect("all verified pages must complete the source receipt");

    let chunk_files = observed_chunks
        .iter()
        .map(|chunk| chunk.anchor.file_occurrence_id.clone())
        .collect::<BTreeSet<_>>();
    let page_import_files = observed_imports
        .iter()
        .map(|evidence| evidence.file_occurrence_id.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(chunk_files.len(), 2, "pages must retain both files' chunks");
    assert_eq!(page_import_files, import_files);
    let mut expected_chunks = generation
        .admitted_chunks()
        .expect("generation admits exact chunks")
        .iter()
        .map(|chunk| chunk.chunk().clone())
        .collect::<Vec<_>>();
    observed_chunks.sort_by(|left, right| left.id.cmp(&right.id));
    expected_chunks.sort_by(|left, right| left.id.cmp(&right.id));
    assert_eq!(observed_chunks, expected_chunks);
    assert_eq!(observed_imports, generation.imports());
}

#[test]
fn verified_sealed_lexical_page_retained_bytes_include_real_owned_capacities() {
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
        .expect("production owner");
    let mut request = request_with_source(
        "file.lexical-import-capacity",
        1_267_000,
        "commit.lexical-import-capacity",
        "tree.lexical-import-capacity",
        "import type { Widget } from \"widget-kit\";\nexport function render(value: Widget) { return value; }\n",
    );
    request.snapshot.files[0].logical_path = "src/imports.ts".to_owned();
    request.snapshot.files[0].language = Some(id::<LanguageId>("typescript"));
    request.changed_files.clear();
    request.changed_files.insert("src/imports.ts".to_owned());
    request
        .snapshot
        .validate()
        .expect("TypeScript import snapshot is canonical");
    let generation = owner
        .build_and_publish(request, &ActiveControl)
        .expect("generation publishes");
    let sealed = generation.encode_sealed().expect("generation seals");
    let envelope: serde_json::Value =
        serde_json::from_slice(&sealed).expect("sealed generation JSON");
    let state_digest = id::<ManifestDigest>(
        envelope["state_digest"]
            .as_str()
            .expect("state digest string"),
    );
    let mut source = VerifiedSealedLexicalPageSourceV1::open(
        Cursor::new(sealed.clone()),
        u64::try_from(sealed.len()).expect("sealed length"),
        state_digest,
        usize::MAX,
        1024 * 1024,
        &ActiveControl,
    )
    .expect("verified page source opens");
    let page = match source.next_page(&ActiveControl).expect("verified page") {
        VerifiedSealedLexicalPageReadV1::Page(page) => page,
        VerifiedSealedLexicalPageReadV1::Complete(_) => panic!("fixture must emit a page"),
    };
    assert!(!page.chunks().is_empty());
    assert!(!page.imports().is_empty());

    let vector_and_module_capacity_floor = page
        .chunk_capacity()
        .saturating_mul(std::mem::size_of::<ExtractionAdmittedCodeSearchChunkV1>())
        .saturating_add(
            page.import_capacity()
                .saturating_mul(std::mem::size_of::<CodeIndexImportEvidenceV1>()),
        )
        .saturating_add(page.imports()[0].module_specifier.capacity());
    assert!(
        page.retained_owned_bytes() >= vector_and_module_capacity_floor,
        "real source page accounting must include its actual vector and string capacities"
    );
}

#[test]
fn verified_sealed_lexical_source_reads_the_legacy_v5_payload_without_full_restore() {
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
        .expect("production owner");
    let generation = owner
        .build_and_publish(request("file.lexical-v5", 1_270_000), &ActiveControl)
        .expect("generation publishes");
    let sealed = generation.encode_sealed().expect("generation seals");
    let mut envelope: serde_json::Value =
        serde_json::from_slice(&sealed).expect("sealed generation JSON");
    envelope["generation"]["format_revision"] = serde_json::Value::from(5);
    let state_digest = sealed_generation_payload_digest(5, &envelope["generation"])
        .expect("legacy payload digest");
    envelope["state_digest"] = serde_json::Value::String(state_digest.as_str().to_owned());
    let legacy = serde_json::to_vec(&envelope).expect("legacy sealed generation JSON");
    let mut source = VerifiedSealedLexicalPageSourceV1::open(
        Cursor::new(legacy.clone()),
        u64::try_from(legacy.len()).expect("legacy sealed length"),
        state_digest,
        64,
        1024 * 1024,
        &ActiveControl,
    )
    .expect("legacy verified page source opens");

    let receipt = loop {
        match source.next_page(&ActiveControl).expect("legacy page read") {
            VerifiedSealedLexicalPageReadV1::Page(page) => {
                assert!(!page.chunks().is_empty() || !page.imports().is_empty())
            }
            VerifiedSealedLexicalPageReadV1::Complete(receipt) => break receipt,
        }
    };
    assert_eq!(receipt.format_revision(), 5);
    assert!(receipt.total_chunks() > 0);
}

/// The published-generation integrity gate is an amortized load-time check.
/// Verifying once per loaded generation must reach exactly the verdict a fresh
/// verification reaches, and must stay fail-closed for a generation that has
/// never validated.
#[test]
fn published_generation_validation_is_amortized_per_loaded_generation() {
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
        .expect("production owner");
    let generation = owner
        .build_and_publish(request("file.validation.memo", 1_300_000), &ActiveControl)
        .expect("valid generation publishes");

    // Publication already ran the full gate, so the generation carries a
    // verified mark and later seals reuse it instead of re-verifying.
    assert!(
        generation.is_validated(),
        "publishing a generation must run its integrity gate"
    );
    let sealed = generation.encode_sealed().expect("valid generation seals");
    let resealed = generation
        .encode_sealed()
        .expect("an already-verified generation reseals");
    assert_eq!(
        sealed, resealed,
        "the memoized gate must reach the same verdict and payload as the first check"
    );

    // Restoring re-reads bytes from the sealed store, so it must verify fresh
    // rather than trust any carried mark.
    let restored =
        CodeIndexPublishedGenerationV1::decode_sealed(&sealed).expect("valid generation restores");
    assert!(
        restored.is_validated(),
        "a restored generation must be fully verified before it can serve"
    );
    assert_eq!(restored.manifest(), generation.manifest());
    assert_eq!(
        restored
            .encode_sealed()
            .expect("restored generation reseals"),
        sealed,
        "a restored generation must reseal to identical bytes"
    );

    // Repeat exact admission is memoized and must stay byte-identical.
    let first_admitted = restored
        .admitted_chunks()
        .expect("parser-backed exact authority");
    let second_admitted = restored
        .admitted_chunks()
        .expect("repeat exact admission is amortized");
    assert!(!first_admitted.is_empty());
    assert_eq!(first_admitted.len(), second_admitted.len());
    assert!(
        first_admitted
            .iter()
            .zip(second_admitted.iter())
            .all(|(first, second)| first.chunk() == second.chunk()),
        "amortized admission must return the same chunks as the first admission"
    );

    // Repeat attribution reads are memoized and must stay identical.
    let first_attribution = restored
        .test_attribution_authority()
        .expect("test attribution authority");
    let second_attribution = restored
        .test_attribution_authority()
        .expect("repeat attribution read is amortized");
    let generation_id = restored.manifest().generation_id.clone();
    assert_eq!(
        format!(
            "{:?}",
            first_attribution.read_test_attribution(&generation_id)
        ),
        format!(
            "{:?}",
            second_attribution.read_test_attribution(&generation_id)
        ),
        "amortized attribution must return the same evidence as the first read"
    );
}

/// Corruption of chunk evidence must still be caught by the very first
/// validation of a generation. Memoizing a verdict must never let a generation
/// that has not validated serve.
#[test]
fn corrupted_chunk_evidence_fails_the_first_validation_of_a_restored_generation() {
    let store = SharedPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
        .expect("production owner");
    let generation = owner
        .build_and_publish(
            request("file.validation.corrupt", 1_400_000),
            &ActiveControl,
        )
        .expect("valid generation publishes");
    let sealed = generation.encode_sealed().expect("valid generation seals");

    let mut envelope: serde_json::Value =
        serde_json::from_slice(&sealed).expect("sealed generation JSON");
    let chunk = &mut envelope["generation"]["files"][0]["artifacts"]["chunks"]["chunks"][0];
    assert!(
        !chunk.is_null(),
        "the fixture generation must contain at least one chunk"
    );
    // Break the chunk's canonical identity so it no longer matches the document
    // membership its file artifact claims.
    chunk["id"] = serde_json::Value::String("chunk.tampered".to_owned());
    // Re-seal the envelope so the outer state digest cannot be what rejects it.
    let state_digest = sealed_generation_payload_digest(
        SEALED_GENERATION_FORMAT_REVISION_V1,
        &envelope["generation"],
    )
    .expect("forged payload has a state digest");
    envelope["state_digest"] = serde_json::Value::String(state_digest.as_str().to_owned());
    let forged = serde_json::to_vec(&envelope).expect("forged sealed generation JSON");

    let error = CodeIndexPublishedGenerationV1::decode_sealed(&forged)
        .expect_err("corrupted chunk evidence must fail the first validation");
    let message = error.to_string();
    assert!(
        message.contains("chunk") || message.contains("digest") || message.contains("canonical"),
        "unexpected corrupted-chunk error: {message}"
    );
}

#[test]
fn linked_worktrees_share_one_repository_store_but_isolate_active_generations() {
    let store = SharedPublicationStore::default();
    let primary_store = store.clone();
    let linked_store = store.clone();
    assert!(primary_store.shares_repository_store(&linked_store));

    let mut owner =
        CodeIndexProductionOwnerV1::new(config(), store.clone(), ApplyingProjectionSink)
            .expect("production owner");
    let primary_request = request_in_scope(
        "file.primary.1",
        1_100_000,
        "refs/heads/main",
        None,
        "commit.main.1",
    );
    let primary_scope = CodeIndexGenerationScopeV1::for_snapshot(&primary_request.snapshot);
    let primary = owner
        .build_and_publish(primary_request, &ActiveControl)
        .expect("primary generation publishes");

    let linked_request = request_in_scope(
        "file.linked.1",
        1_200_000,
        "refs/heads/feature",
        Some("worktree.feature"),
        "commit.feature.1",
    );
    let linked_scope = CodeIndexGenerationScopeV1::for_snapshot(&linked_request.snapshot);
    let linked = owner
        .build_and_publish(linked_request, &ActiveControl)
        .expect("linked-worktree generation publishes");

    assert_ne!(primary_scope, linked_scope);
    assert_ne!(
        primary.manifest().generation_id,
        linked.manifest().generation_id
    );
    assert!(linked.manifest().parent_generation.is_none());
    assert_eq!(store.scope_count(), 2);
    assert_eq!(
        store
            .load_active(&primary_scope)
            .expect("primary scope read")
            .expect("primary remains active")
            .manifest(),
        primary.manifest()
    );
    assert_eq!(
        store
            .load_active(&linked_scope)
            .expect("linked scope read")
            .expect("linked remains active")
            .manifest(),
        linked.manifest()
    );
}

#[test]
fn linked_worktree_no_op_reuses_only_its_compatible_generation() {
    let store = SharedPublicationStore::default();
    let mut owner =
        CodeIndexProductionOwnerV1::new(config(), store.clone(), ApplyingProjectionSink)
            .expect("production owner");
    let first = owner
        .build_and_publish(
            request_in_scope(
                "file.linked.1",
                1_100_000,
                "refs/heads/feature",
                Some("worktree.feature"),
                "commit.feature.1",
            ),
            &ActiveControl,
        )
        .expect("first linked-worktree generation");
    let second = owner
        .build_and_publish(
            request_in_scope(
                "file.linked.2",
                1_200_000,
                "refs/heads/feature",
                Some("worktree.feature"),
                "commit.feature.1",
            ),
            &ActiveControl,
        )
        .expect("no-op linked-worktree generation");

    assert_eq!(
        second.manifest().parent_generation,
        Some(first.manifest().generation_id.clone())
    );
    assert!(
        second
            .projection()
            .request()
            .changes
            .added_or_changed
            .is_empty()
    );
    assert!(second.projection().request().changes.deleted.is_empty());
    assert!(!second.projection().request().changes.reused.is_empty());
    assert_eq!(store.scope_count(), 1);
}

/// A checkout misclassified as "not a git repository" resolves a scope
/// without a reference or worktree while the store still answers with the
/// sealed git-identified generation. That answer must be one terminal typed
/// reset state — never the generic contract error callers can only retry on
/// a one-second cadence — and the sealed generation's git identity must
/// survive the refusal untouched.
#[test]
fn misclassified_non_git_scope_reaches_a_terminal_reset_state_instead_of_retrying() {
    let store = PartialKeyPublicationStore::default();
    let mut owner =
        CodeIndexProductionOwnerV1::new(config(), store.clone(), ApplyingProjectionSink)
            .expect("production owner");
    let hawk_request = request_in_scope(
        "file.identity.hawk",
        1_100_000,
        "refs/heads/hawk",
        Some("worktree.primary"),
        "commit.hawk.1",
    );
    let hawk_scope = CodeIndexGenerationScopeV1::for_snapshot(&hawk_request.snapshot);
    let sealed = owner
        .build_and_publish(hawk_request, &ActiveControl)
        .expect("git-identified generation publishes");

    let non_git = request("file.identity.non-git", 1_200_000);
    let non_git_scope = CodeIndexGenerationScopeV1::for_snapshot(&non_git.snapshot);
    assert_eq!(non_git_scope.reference, None);
    assert_eq!(non_git_scope.worktree, None);

    let read = owner
        .active_generation(&non_git_scope)
        .expect_err("a git-identified generation never dispatches onto a non-git scope");
    assert!(
        matches!(
            &read,
            CodeIndexProductionErrorV1::Publication(
                CodeIndexPublicationStoreErrorV1::CorruptionResetRequired(_)
            )
        ),
        "the refusal must be the terminal reset state, not a retryable error: {read}"
    );

    let rebuild = owner
        .build_and_publish(non_git, &ActiveControl)
        .expect_err("a reconcile under the misclassified identity refuses terminally");
    assert!(
        matches!(
            &rebuild,
            CodeIndexProductionErrorV1::Publication(
                CodeIndexPublicationStoreErrorV1::CorruptionResetRequired(_)
            )
        ),
        "repeat reconciles reach the same terminal state instead of spinning: {rebuild}"
    );

    // The sealed generation and its real git identity survive the refusal.
    let retained = store
        .load_active(&hawk_scope)
        .expect("read publication state")
        .expect("the sealed generation is never dropped by the refusal");
    assert_eq!(retained.manifest(), sealed.manifest());
    assert_eq!(retained.sealed_scope(), hawk_scope);
}

/// The complement of the terminal refusal: a genuinely non-git snapshot is a
/// first-class terminal outcome. It indexes as its own genesis slot beside
/// git-identified generations instead of erroring or adopting one of them.
#[test]
fn non_git_scope_stays_independently_active_beside_git_identified_generations() {
    let store = SharedPublicationStore::default();
    let mut owner =
        CodeIndexProductionOwnerV1::new(config(), store.clone(), ApplyingProjectionSink)
            .expect("production owner");
    let hawk_request = request_in_scope(
        "file.terminal.hawk",
        1_100_000,
        "refs/heads/hawk",
        Some("worktree.primary"),
        "commit.hawk.1",
    );
    let hawk_scope = CodeIndexGenerationScopeV1::for_snapshot(&hawk_request.snapshot);
    let hawk = owner
        .build_and_publish(hawk_request, &ActiveControl)
        .expect("git-identified generation publishes");

    let non_git_request = request("file.terminal.non-git", 1_200_000);
    let non_git_scope = CodeIndexGenerationScopeV1::for_snapshot(&non_git_request.snapshot);
    let non_git = owner
        .build_and_publish(non_git_request, &ActiveControl)
        .expect("a non-git snapshot indexes as its own terminal outcome");

    assert!(non_git.manifest().parent_generation.is_none());
    assert_eq!(store.scope_count(), 2);
    assert_eq!(
        owner
            .active_generation(&hawk_scope)
            .expect("hawk scope read")
            .expect("hawk stays active")
            .manifest(),
        hawk.manifest()
    );
    assert_eq!(
        owner
            .active_generation(&non_git_scope)
            .expect("non-git scope read")
            .expect("non-git stays active")
            .manifest(),
        non_git.manifest()
    );
}

/// Config dispatch is full-scope exact. A store matching on a partial key
/// (repository-only pointer) must never get a generation sealed for one
/// branch/worktree dispatched onto another scope — neither onto a different
/// worktree (the PR checkout) nor onto the same worktree under a different
/// sealed branch label.
#[test]
fn config_dispatch_refuses_a_generation_sealed_for_another_full_scope() {
    let store = PartialKeyPublicationStore::default();
    let mut owner = CodeIndexProductionOwnerV1::new(config(), store, ApplyingProjectionSink)
        .expect("production owner");
    let hawk_request = request_in_scope(
        "file.dispatch.hawk",
        1_100_000,
        "refs/heads/hawk",
        Some("worktree.hawk"),
        "commit.hawk.1",
    );
    let hawk_scope = CodeIndexGenerationScopeV1::for_snapshot(&hawk_request.snapshot);
    let hawk = owner
        .build_and_publish(hawk_request, &ActiveControl)
        .expect("hawk generation publishes");

    let pr_worktree_scope = CodeIndexGenerationScopeV1 {
        repository: hawk_scope.repository.clone(),
        reference: Some(id::<RefId>("refs/heads/pr-1234")),
        worktree: Some(id::<WorktreeId>("worktree.pr")),
    };
    let moved_label_scope = CodeIndexGenerationScopeV1 {
        repository: hawk_scope.repository.clone(),
        reference: Some(id::<RefId>("refs/heads/pr-1234")),
        worktree: hawk_scope.worktree.clone(),
    };
    for foreign_scope in [&pr_worktree_scope, &moved_label_scope] {
        let refused = owner
            .active_generation(foreign_scope)
            .expect_err("a generation sealed for hawk never dispatches onto another scope");
        assert!(
            matches!(
                &refused,
                CodeIndexProductionErrorV1::Publication(
                    CodeIndexPublicationStoreErrorV1::CorruptionResetRequired(_)
                )
            ),
            "full-scope dispatch mismatch must be the terminal reset state: {refused}"
        );
    }

    // The exact sealed full scope still dispatches.
    assert_eq!(
        owner
            .active_generation(&hawk_scope)
            .expect("exact scope read")
            .expect("hawk stays active for its own scope")
            .manifest(),
        hawk.manifest()
    );
}

/// The code shard/slot key is the sealed branch label inside the full scope —
/// never a filesystem path (the scope carries none) and never the generation
/// id: successive generations share their branch's slot while a second branch
/// on the same worktree splits into its own independently active slot.
#[test]
fn code_shard_slot_key_is_the_sealed_branch_label_not_a_generation_id() {
    let store = SharedPublicationStore::default();
    let mut owner =
        CodeIndexProductionOwnerV1::new(config(), store.clone(), ApplyingProjectionSink)
            .expect("production owner");
    let first = owner
        .build_and_publish(
            request_in_scope(
                "file.shard.hawk.1",
                1_100_000,
                "refs/heads/hawk",
                Some("worktree.shared"),
                "commit.hawk.1",
            ),
            &ActiveControl,
        )
        .expect("first hawk generation publishes");
    let second = owner
        .build_and_publish(
            request_in_scope(
                "file.shard.hawk.2",
                1_200_000,
                "refs/heads/hawk",
                Some("worktree.shared"),
                "commit.hawk.1",
            ),
            &ActiveControl,
        )
        .expect("second hawk generation publishes");

    // A new generation id under the same sealed branch label reuses the slot.
    assert_ne!(
        first.manifest().generation_id,
        second.manifest().generation_id
    );
    assert_eq!(second.sealed_scope(), first.sealed_scope());
    assert_eq!(store.scope_count(), 1);

    // The same worktree under another sealed branch label is its own slot.
    let pr = owner
        .build_and_publish(
            request_in_scope(
                "file.shard.pr.1",
                1_300_000,
                "refs/heads/pr-1234",
                Some("worktree.shared"),
                "commit.pr.1",
            ),
            &ActiveControl,
        )
        .expect("pr generation publishes");
    assert_eq!(pr.sealed_scope().worktree, first.sealed_scope().worktree);
    assert_ne!(pr.sealed_scope(), first.sealed_scope());
    assert_eq!(
        pr.sealed_scope().reference,
        Some(id::<RefId>("refs/heads/pr-1234"))
    );
    assert!(pr.manifest().parent_generation.is_none());
    assert_eq!(store.scope_count(), 2);
    assert_eq!(
        owner
            .active_generation(&second.sealed_scope())
            .expect("hawk slot read")
            .expect("hawk slot stays active")
            .manifest(),
        second.manifest()
    );

    // The sealed branch label is durable through the sealed codec, so a
    // restored generation still names the exact slot it was sealed for.
    let restored = CodeIndexPublishedGenerationV1::decode_sealed(
        &pr.encode_sealed().expect("pr generation seals"),
    )
    .expect("pr generation restores");
    assert_eq!(restored.sealed_scope(), pr.sealed_scope());
}

#[test]
fn branch_stack_nodes_and_snapshots_derive_the_same_path_free_scope() {
    let request = request_in_scope(
        "file.branch-stack.1",
        1_100_000,
        "refs/heads/feature",
        Some("worktree.feature"),
        "commit.feature.1",
    );
    let node = BranchStackNodeV1 {
        node_id: id::<StackNodeId>("stack-node.feature"),
        project_id: id("project.fixture"),
        repository_id: request.snapshot.repository.clone(),
        reference: request
            .snapshot
            .reference
            .clone()
            .expect("branch reference"),
        tip: request
            .snapshot
            .source_revision
            .clone()
            .expect("branch tip"),
        worktree_id: request.snapshot.worktree.clone(),
    };

    assert_eq!(
        CodeIndexGenerationScopeV1::for_branch_stack_node(&node),
        CodeIndexGenerationScopeV1::for_snapshot(&request.snapshot)
    );
}

#[test]
fn production_owner_abstains_without_publication_on_cancellation_or_deadline() {
    let store = SharedPublicationStore::default();
    let mut owner =
        CodeIndexProductionOwnerV1::new(config(), store.clone(), ApplyingProjectionSink)
            .expect("production owner");

    let cancelled = owner
        .build_and_publish(
            request("file.production.cancelled", 1_100_000),
            &CancelledControl,
        )
        .expect_err("cancelled run must not publish");
    assert!(matches!(
        cancelled,
        CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::Cancelled)
    ));
    assert!(
        store
            .load_active(&CodeIndexGenerationScopeV1::for_snapshot(
                &request("file.production.cancelled.scope", 1_100_000).snapshot,
            ))
            .expect("read publication state")
            .is_none()
    );

    let expired = owner
        .build_and_publish(
            request("file.production.expired", 1_100_000),
            &ExpiredControl,
        )
        .expect_err("expired run must not publish");
    assert!(matches!(
        expired,
        CodeIndexProductionErrorV1::Interrupted(CodeIndexInterruptionV1::DeadlineExceeded)
    ));
    assert!(
        store
            .load_active(&CodeIndexGenerationScopeV1::for_snapshot(
                &request("file.production.expired.scope", 1_100_000).snapshot,
            ))
            .expect("read publication state")
            .is_none()
    );
}

#[test]
fn production_owner_never_activates_a_generation_after_projection_failure() {
    let store = SharedPublicationStore::default();
    let mut owner =
        CodeIndexProductionOwnerV1::new(config(), store.clone(), RejectingProjectionSink)
            .expect("production owner");

    let error = owner
        .build_and_publish(
            request("file.production.rejected", 1_100_000),
            &ActiveControl,
        )
        .expect_err("rejected projection must not publish");
    assert!(matches!(error, CodeIndexProductionErrorV1::Projection(_)));
    assert!(
        store
            .load_active(&CodeIndexGenerationScopeV1::for_snapshot(
                &request("file.production.rejected.scope", 1_100_000).snapshot,
            ))
            .expect("read publication state")
            .is_none()
    );
}

/// Width is sizing policy, never semantics. A generation built with the
/// per-file sweep running inline must be byte-identical to one built at full
/// machine width — same manifest, same chunks, same digests, same order.
#[test]
fn parallel_and_sequential_generations_are_byte_identical() {
    parallel_equivalence::assert_parallel_and_sequential_generations_are_byte_identical();
}
