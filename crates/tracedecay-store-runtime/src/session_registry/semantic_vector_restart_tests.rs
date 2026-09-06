use std::path::Path;
use std::process::Command;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use tracedecay_code_index_retention::code_index_generations::DurablePublicationPointerV1;
use tracedecay_code_index_runtime::CodeGraphReplayBindingV1;
use tracedecay_code_index_runtime::code_graph_seat::CodeGraphSeatLeaseV1;
use tracedecay_code_index_runtime::code_index_scheduler::{
    CodeIndexWorktreeSchedulerV1, SharedCodeIndexBytePoolV1, scoped_code_index_store_root,
};
use tracedecay_daemon_identity::profile_identity;
use tracedecay_domain::{
    ChangedCodeChunkSetV1, ChangedCodeChunkV1, ChunkerRevision, CodeGenerationId,
    CodeSearchChunkId, ContentDigest, EmbeddingDeviceClassV1, EmbeddingDocumentCompositionV1,
    EmbeddingMetricV1, EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1,
    EmbeddingProjectionKeyV1, EmbeddingTruncationSideV1, PrivacyDomainId, ProjectId,
    ProjectionBatchRequestV1, ProjectionOperationV1, ProjectionOutcomeV1, ProjectionReplayReasonV1,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphWriteBatch, NeverCancelled,
    VerifiedGenerationBatchCommit, VerifiedGenerationBeginV1, VerifiedGraphSnapshot,
};
use tracedecay_semantic::projector::PreparedVectorGenerationV1;
use tracedecay_semantic::projector::{ProjectedChunkVectorV1, vector_output_digest};
use tracedecay_store::{
    CodeShardScopeV1, GraphPublicationKeyV1, GraphVerifiedHeadV1,
    SemanticVectorPublishedGenerationDependencyLookup, SemanticVectorPublishedGenerationKey,
    SemanticVectorPublishedGenerationLookup, SemanticVectorSourceScopeBindingLookup,
    SemanticVectorStageBatchReceipt, SemanticVectorStageCancelOutcome, SemanticVectorStageKey,
    SemanticVectorStagePlan, SemanticVectorStagePublicationPrepareOutcome,
    SemanticVectorStagePublishOutcome, SemanticVectorStagePublishSettlement,
    SemanticVectorStageResumeOutcome, StoreRuntimeBindingV1, StoreShardIdV1,
};
use tracedecay_usecases::semantic_runtime::{
    RetainedSemanticVectorGraphV1, SemanticGraphExecutionAuthorityV1, SemanticVectorGraphScopeV1,
    SemanticVectorRetentionAuthorizationV1, VerifiedSemanticVectorGraphRuntimeV1,
};
use tracedecay_usecases::store::vector_generations::{
    GraphVectorGenerationStoreV1, SemanticVectorStageDescriptorV1, VectorGenerationPlanV1,
};

use super::DaemonSessionRuntimeRegistryV1;
use super::code_graph::RetainedCodeGraphRuntimeV1;

struct AtomicCancellation(Arc<AtomicBool>);

impl GraphCancellation for AtomicCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

struct CancelRequestAfterAdoptionRuntime {
    inner: Arc<dyn VerifiedSemanticVectorGraphRuntimeV1>,
    request_cancelled: Arc<AtomicBool>,
}

impl VerifiedSemanticVectorGraphRuntimeV1 for CancelRequestAfterAdoptionRuntime {
    fn scope(&self) -> &SemanticVectorGraphScopeV1 {
        self.inner.scope()
    }

    fn recover_verified_snapshot(
        &self,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
        self.inner.recover_verified_snapshot(authority)
    }

    fn recover_verified_generation(
        &self,
        publication: &GraphPublicationKeyV1,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        self.inner
            .recover_verified_generation(publication, authority)
    }

    fn staging_binding(&self) -> (&StoreShardIdV1, &StoreRuntimeBindingV1) {
        self.inner.staging_binding()
    }

    fn verified_head(
        &self,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<Option<GraphVerifiedHeadV1>, GraphDbError> {
        self.inner.verified_head(authority)
    }

    fn begin_stage(
        &self,
        plan: &SemanticVectorStagePlan,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VerifiedGenerationBeginV1, GraphDbError> {
        self.inner.begin_stage(plan, authority)
    }

    fn resume_stage(
        &self,
        stage: &SemanticVectorStageKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStageResumeOutcome, GraphDbError> {
        let outcome = self.inner.resume_stage(stage, authority)?;
        if matches!(outcome, SemanticVectorStageResumeOutcome::Pending(_)) {
            self.request_cancelled.store(true, Ordering::Release);
        }
        Ok(outcome)
    }

    fn published_semantic_generation(
        &self,
        key: &SemanticVectorPublishedGenerationKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorPublishedGenerationLookup, GraphDbError> {
        self.inner.published_semantic_generation(key, authority)
    }

    fn append_stage_batch(
        &self,
        receipt: &SemanticVectorStageBatchReceipt,
        batch: GraphWriteBatch,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VerifiedGenerationBatchCommit, GraphDbError> {
        self.inner.append_stage_batch(receipt, batch, authority)
    }

    fn cancel_stage(
        &self,
        stage: &SemanticVectorStageKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStageCancelOutcome, GraphDbError> {
        self.inner.cancel_stage(stage, authority)
    }

    fn prepare_publication_from_staged_native(
        &self,
        stage: &SemanticVectorStageKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStagePublicationPrepareOutcome, GraphDbError> {
        self.inner
            .prepare_publication_from_staged_native(stage, authority)
    }

    fn publish_ready_stage(
        &self,
        stage: &SemanticVectorStageKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        self.inner.publish_ready_stage(stage, authority)
    }

    fn settle_published(
        &self,
        settlement: &SemanticVectorStagePublishSettlement,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStagePublishOutcome, GraphDbError> {
        self.inner.settle_published(settlement, authority)
    }

    fn reserve_one_generation(
        &self,
        after: Option<tracedecay_store::SemanticVectorStageCensusCursor>,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<tracedecay_graph_db::SemanticVectorRetentionStep, GraphDbError> {
        self.inner.reserve_one_generation(after, authority)
    }

    fn finalize_reserved_generation(
        &self,
        reservation: tracedecay_graph_db::SemanticVectorRetirementReservation,
        authorization: &SemanticVectorRetentionAuthorizationV1,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<tracedecay_graph_db::SemanticVectorRetentionAction, GraphDbError> {
        self.inner
            .finalize_reserved_generation(reservation, authorization, authority)
    }

    fn release_reserved_generation(
        &self,
        reservation: tracedecay_graph_db::SemanticVectorRetirementReservation,
    ) -> Result<(), GraphDbError> {
        self.inner.release_reserved_generation(reservation)
    }

    fn source_generation_has_live_reference(
        &self,
        generation: &tracedecay_store::SemanticVectorSourceGenerationId,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<bool, GraphDbError> {
        self.inner
            .source_generation_has_live_reference(generation, expected_revision, authority)
    }

    fn source_scope_has_live_reference(
        &self,
        source_scope: &StoreShardIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<bool, GraphDbError> {
        self.inner
            .source_scope_has_live_reference(source_scope, expected_revision, authority)
    }

    fn published_generation_dependency(
        &self,
        generation: &tracedecay_domain::VectorGenerationIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<tracedecay_store::SemanticVectorPublishedGenerationDependencyLookup, GraphDbError>
    {
        self.inner
            .published_generation_dependency(generation, expected_revision, authority)
    }

    fn validate_project_census_revision(
        &self,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<(), GraphDbError> {
        self.inner
            .validate_project_census_revision(expected_revision, authority)
    }

    fn source_scope_binding(
        &self,
        code_scope_hash: &tracedecay_store::SemanticVectorCodeScopeHash,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<tracedecay_store::SemanticVectorSourceScopeBindingLookup, GraphDbError> {
        self.inner
            .source_scope_binding(code_scope_hash, expected_revision, authority)
    }

    fn remove_source_scope_binding(
        &self,
        code_scope_hash: &tracedecay_store::SemanticVectorCodeScopeHash,
        source_scope: &StoreShardIdV1,
        expected_revision: tracedecay_store::SemanticVectorStageCensusRevision,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<bool, GraphDbError> {
        self.inner.remove_source_scope_binding(
            code_scope_hash,
            source_scope,
            expected_revision,
            authority,
        )
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .expect("run git fixture command");
    assert!(
        output.status.success(),
        "git fixture command failed: {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn admitted_embedding() -> tracedecay_domain::AdmittedEmbeddingProjectionKeyV1 {
    EmbeddingProjectionKeyV1 {
        model_artifact_digest: digest('e'),
        tokenizer_digest: digest('2'),
        config_digest: digest('3'),
        query_instruction_digest: None,
        document_instruction_digest: None,
        document_composition: EmbeddingDocumentCompositionV1::SanitizedText,
        pooling: EmbeddingPoolingV1::Mean,
        truncation_side: EmbeddingTruncationSideV1::Right,
        truncation_length: 512,
        inference_batch_size: 8,
        inference_batch_bytes: 16 * 1024,
        runtime_backend: "semantic-vector-restart-runtime".to_owned(),
        runtime_build_revision: "semantic-vector-restart-runtime.v1".to_owned(),
        device_class: EmbeddingDeviceClassV1::Cpu,
        dimensions: 1,
        normalization: EmbeddingNormalizationV1::L2,
        metric: EmbeddingMetricV1::Cosine,
        precision: EmbeddingPrecisionV1::Fp32,
        chunk_schema_revision: "code-search-chunk.v1".to_owned(),
        chunker_revision: ChunkerRevision::new("chunker.semantic-vector-restart.v1")
            .expect("chunker revision"),
        privacy_domain: PrivacyDomainId::new("privacy.semantic-vector-restart")
            .expect("privacy domain"),
        privacy_key_epoch: 1,
    }
    .admit()
    .expect("admitted embedding")
}

fn prepared_generation(
    source: &CodeGenerationId,
    chunk: &str,
    digest_byte: char,
) -> (
    VectorGenerationPlanV1,
    PreparedVectorGenerationV1,
    SemanticVectorStageDescriptorV1,
) {
    let embedding = admitted_embedding();
    let chunk_id = CodeSearchChunkId::new(chunk).expect("chunk id");
    let chunk_digest = ContentDigest::new(format!("sha256:{}", digest_byte.to_string().repeat(64)))
        .expect("chunk digest");
    let mut changes = ChangedCodeChunkSetV1 {
        from_generation: None,
        to_generation: source.clone(),
        manifest_digest: digest('0'),
        added_or_changed: vec![ChangedCodeChunkV1 {
            chunk_id: chunk_id.clone(),
            prior_digest: None,
            current_digest: Some(chunk_digest.clone()),
        }],
        deleted: vec![],
        reused: vec![],
    };
    changes.manifest_digest = changes.compute_digest().expect("change manifest digest");
    let descriptor = SemanticVectorStageDescriptorV1::from_changes(embedding.clone(), &changes)
        .expect("semantic stage descriptor");
    let mut request = ProjectionBatchRequestV1 {
        request_digest: digest('0'),
        changes,
        previous_projection_key: None,
        target_projection_key: embedding.projection_key().clone(),
        replay_reason: ProjectionReplayReasonV1::FullRebuildIncompatible,
    };
    request.request_digest = tracedecay_code_index::projection::expected_request_digest(&request)
        .expect("projection request digest");
    let values = vec![f32::from(digest_byte as u8)];
    let output_digest = vector_output_digest(
        embedding.projection_key(),
        &chunk_id,
        &chunk_digest,
        &values,
    )
    .expect("vector output digest");
    let receipt = tracedecay_code_index::projection::build_batch_receipt(
        &request,
        &[
            tracedecay_code_index::projection::ChunkProjectionDecisionV1 {
                chunk_id: chunk_id.clone(),
                prior_chunk_digest: None,
                current_chunk_digest: Some(chunk_digest.clone()),
                operation: ProjectionOperationV1::Added,
                outcome: ProjectionOutcomeV1::Applied,
                output_digest: Some(output_digest.clone()),
            },
        ],
    )
    .expect("projection receipt");
    let prepared = PreparedVectorGenerationV1 {
        embedding_key: embedding.clone(),
        request: request.clone(),
        receipt,
        vectors: vec![ProjectedChunkVectorV1 {
            projection_key: embedding.projection_key().clone(),
            source_generation: source.clone(),
            source_manifest_digest: request.changes.manifest_digest.clone(),
            chunk_id: chunk_id.clone(),
            chunk_digest,
            values,
            output_digest,
        }],
        tombstones: vec![],
    };
    let plan = VectorGenerationPlanV1 {
        target_projection_key: embedding.projection_key().clone(),
        source_generation: source.clone(),
        source_manifest_digest: request.changes.manifest_digest,
        expected_chunk_ids: vec![chunk_id].into(),
        base_generation: None,
    };
    (plan, prepared, descriptor)
}

fn digest<T: TryFrom<String>>(byte: char) -> T
where
    T::Error: std::fmt::Debug,
{
    T::try_from(format!("sha256:{}", byte.to_string().repeat(64))).expect("test digest")
}

fn retain_semantic_graph(
    runtime: RetainedCodeGraphRuntimeV1,
    project_root: &Path,
) -> RetainedSemanticVectorGraphV1 {
    let (project, repository, worktree, source_generation, source_dependency) = runtime
        .semantic_vector_identity()
        .expect("semantic vector identity");
    let scope = SemanticVectorGraphScopeV1::new(
        project,
        repository,
        worktree,
        source_generation,
        tracedecay_store::SemanticVectorCodeScopeHash::new(
            tracedecay_code_index_retention::code_index_generations::code_index_scope_hash(
                project_root,
            ),
        )
        .expect("code scope hash"),
        source_dependency,
    )
    .expect("semantic vector graph scope");
    let runtime = Box::new(runtime).into_semantic_vector_runtime(scope);
    RetainedSemanticVectorGraphV1::new(runtime, Arc::new(NeverCancelled))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prior_daemon_pending_stage_is_adopted_and_replacement_publishes() {
    let temporary = tempfile::tempdir().expect("semantic restart fixture root");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let profile_root = root.join("profile");
    let project_root = root.join("project");
    std::fs::create_dir_all(project_root.join("src")).expect("project source directory");
    git(&project_root, &["init", "-q", "-b", "main"]);
    git(&project_root, &["config", "user.name", "TraceDecay Test"]);
    git(
        &project_root,
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    std::fs::write(
        project_root.join("src/lib.rs"),
        "pub fn semantic_restart_value() -> usize { 1 }\n",
    )
    .expect("project source");
    git(&project_root, &["add", "."]);
    git(
        &project_root,
        &["commit", "-qm", "semantic restart fixture"],
    );
    let project_id = ProjectId::new("project.semantic-vector-restart").expect("project id");
    tracedecay_runtime_core::storage::pin_fixture_repository_identity(
        &project_root,
        project_id.as_str(),
    )
    .expect("project enrollment");
    let canonical_project = project_root.canonicalize().expect("canonical project root");
    let store_root = root.join("code-index-store");
    let scoped_store = scoped_code_index_store_root(&store_root, &canonical_project);
    let mut scheduler = CodeIndexWorktreeSchedulerV1::open(
        project_id.clone(),
        &canonical_project,
        scoped_store.clone(),
        Arc::new(SharedCodeIndexBytePoolV1::default()),
    )
    .expect("open worktree scheduler");
    scheduler.reconcile_now().expect("seal code generation");
    let latest = scheduler
        .latest_complete()
        .expect("complete code generation");
    let source = latest.generation().manifest().generation_id.clone();
    let pointer: DurablePublicationPointerV1 = serde_json::from_slice(
        &std::fs::read(scoped_store.join("active-code-generation-v1.json"))
            .expect("active code generation pointer"),
    )
    .expect("decode active generation pointer");
    let replay_binding = || CodeGraphReplayBindingV1 {
        generations_root: scoped_store.join("code-generations-v1"),
        sealed_state_digest: tracedecay_graph_db::SealedGraphStateDigest::try_from(
            pointer.state_digest.clone(),
        )
        .expect("sealed state digest"),
    };
    let identity = profile_identity::load_or_create(&profile_root).expect("profile identity");

    let first_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        71,
        "semantic vector restart first daemon",
    )
    .expect("first daemon database scope");
    let first_registry = DaemonSessionRuntimeRegistryV1::open(identity.clone())
        .await
        .expect("first daemon registry");
    let first_database = first_registry
        .project_memory(project_id.clone(), [canonical_project.clone()])
        .await
        .expect("first project database");
    let first_runtime = first_registry
        .retain_code_graph_runtime(
            project_id.clone(),
            latest.generation().snapshot().repository.clone(),
            scheduler.identity().worktree_id().clone(),
            latest.generation().snapshot().reference.clone(),
            source.clone(),
            Arc::clone(&first_database),
            replay_binding(),
            None,
        )
        .await
        .expect("first code graph runtime");
    first_runtime
        .publish_verified_snapshot(latest.generation(), Arc::new(AtomicBool::new(false)))
        .expect("publish source code graph");
    let first_retained = retain_semantic_graph(first_runtime, &canonical_project);
    let first_binding = first_retained.runtime().staging_binding().1.clone();
    let (first_plan, first_prepared, first_descriptor) =
        prepared_generation(&source, "chunk.before-restart", 'a');
    let first_store = GraphVectorGenerationStoreV1::open(&first_retained)
        .await
        .expect("open first semantic vector store");
    first_store
        .configure_stage(first_descriptor)
        .expect("configure first semantic stage");
    let first_build = first_store
        .begin_generation(first_plan, Arc::new(NeverCancelled))
        .await
        .expect("begin first semantic generation")
        .build_id()
        .clone();
    first_store
        .commit_batch(&first_build, None, first_prepared, Arc::new(NeverCancelled))
        .await
        .expect("commit pending first semantic generation");
    drop((
        first_store,
        first_retained,
        first_database,
        first_registry,
        first_scope,
    ));

    let restarted_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        72,
        "semantic vector restart second daemon",
    )
    .expect("restarted daemon database scope");
    let restarted_registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("restarted daemon registry");
    let restarted_database = restarted_registry
        .project_memory(project_id.clone(), [canonical_project.clone()])
        .await
        .expect("restarted project database");
    let restarted_runtime = restarted_registry
        .retain_code_graph_runtime(
            project_id,
            latest.generation().snapshot().repository.clone(),
            scheduler.identity().worktree_id().clone(),
            latest.generation().snapshot().reference.clone(),
            source.clone(),
            Arc::clone(&restarted_database),
            replay_binding(),
            None,
        )
        .await
        .expect("restarted code graph runtime");
    restarted_runtime
        .publish_verified_snapshot(latest.generation(), Arc::new(AtomicBool::new(false)))
        .expect("recover source code graph");
    let restarted_retained = retain_semantic_graph(restarted_runtime, &canonical_project);
    let restarted_binding = restarted_retained.runtime().staging_binding().1.clone();
    assert_ne!(first_binding, restarted_binding);
    let request_cancelled = Arc::new(AtomicBool::new(false));
    let cancellation: Arc<dyn GraphCancellation> =
        Arc::new(AtomicCancellation(Arc::clone(&request_cancelled)));
    let cancellation_probe: Arc<dyn VerifiedSemanticVectorGraphRuntimeV1> =
        Arc::new(CancelRequestAfterAdoptionRuntime {
            inner: Arc::clone(restarted_retained.runtime()),
            request_cancelled: Arc::clone(&request_cancelled),
        });
    let cancellation_retained =
        RetainedSemanticVectorGraphV1::new(cancellation_probe, Arc::new(NeverCancelled));
    let (replacement_plan, replacement_prepared, replacement_descriptor) =
        prepared_generation(&source, "chunk.after-restart", 'b');
    let replacement_store = GraphVectorGenerationStoreV1::open(&cancellation_retained)
        .await
        .expect("open restarted semantic vector store");
    replacement_store
        .configure_stage(replacement_descriptor)
        .expect("configure replacement semantic stage");
    let replacement_build = replacement_store
        .begin_generation(replacement_plan, cancellation)
        .await
        .expect("complete supersession after request cancellation follows adoption")
        .build_id()
        .clone();
    assert!(request_cancelled.load(Ordering::Acquire));
    replacement_store
        .commit_batch(
            &replacement_build,
            None,
            replacement_prepared,
            Arc::new(NeverCancelled),
        )
        .await
        .expect("commit replacement semantic generation");
    let publication = replacement_store
        .publish_generation(&replacement_build, Arc::new(NeverCancelled))
        .await
        .expect("publish replacement semantic generation");
    assert_eq!(publication.checkpoint.source_generation, source);
    drop((
        replacement_store,
        cancellation_retained,
        restarted_retained,
        restarted_database,
        restarted_registry,
        restarted_scope,
        scheduler,
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn linked_worktrees_keep_exact_semantic_bindings_and_publication_dependencies() {
    let temporary = tempfile::tempdir().expect("semantic worktree fixture root");
    let root = temporary
        .path()
        .canonicalize()
        .expect("canonical fixture root");
    let profile_root = root.join("profile");
    let primary_root = root.join("primary");
    let first_root = root.join("linked-first");
    let second_root = root.join("linked-second");
    std::fs::create_dir_all(primary_root.join("src")).expect("primary source directory");
    git(&primary_root, &["init", "-q", "-b", "main"]);
    git(&primary_root, &["config", "user.name", "TraceDecay Test"]);
    git(
        &primary_root,
        &["config", "user.email", "tracedecay@example.invalid"],
    );
    std::fs::write(
        primary_root.join("src/lib.rs"),
        "pub fn linked_semantic_value() -> usize { 0 }\n",
    )
    .expect("primary source");
    git(&primary_root, &["add", "."]);
    git(&primary_root, &["commit", "-qm", "linked semantic fixture"]);
    git(
        &primary_root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "semantic-first",
            first_root.to_str().expect("UTF-8 first worktree"),
        ],
    );
    git(
        &primary_root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "semantic-second",
            second_root.to_str().expect("UTF-8 second worktree"),
        ],
    );
    for (worktree, value) in [(&first_root, 1), (&second_root, 2)] {
        std::fs::write(
            worktree.join("src/lib.rs"),
            format!("pub fn linked_semantic_value() -> usize {{ {value} }}\n"),
        )
        .expect("linked source");
        git(worktree, &["add", "src/lib.rs"]);
        git(
            worktree,
            &["commit", "-qm", "distinct linked semantic source"],
        );
    }

    let project_id = ProjectId::new("project.semantic-linked-worktrees").expect("project id");
    for project_root in [&primary_root, &first_root, &second_root] {
        tracedecay_runtime_core::storage::pin_fixture_repository_identity(
            project_root,
            project_id.as_str(),
        )
        .expect("project enrollment");
    }
    let first_root = first_root.canonicalize().expect("canonical first worktree");
    let second_root = second_root
        .canonicalize()
        .expect("canonical second worktree");
    let store_root = root.join("code-index-store");
    let byte_pool = Arc::new(SharedCodeIndexBytePoolV1::default());
    let first_store_root = scoped_code_index_store_root(&store_root, &first_root);
    let second_store_root = scoped_code_index_store_root(&store_root, &second_root);
    let mut first_scheduler = CodeIndexWorktreeSchedulerV1::open(
        project_id.clone(),
        &first_root,
        first_store_root.clone(),
        Arc::clone(&byte_pool),
    )
    .expect("open first worktree scheduler");
    let mut second_scheduler = CodeIndexWorktreeSchedulerV1::open(
        project_id.clone(),
        &second_root,
        second_store_root.clone(),
        byte_pool,
    )
    .expect("open second worktree scheduler");
    first_scheduler
        .reconcile_now()
        .expect("seal first code generation");
    second_scheduler
        .reconcile_now()
        .expect("seal second code generation");
    let first_generation = first_scheduler
        .latest_complete()
        .expect("first complete generation")
        .generation_handle();
    let second_generation = second_scheduler
        .latest_complete()
        .expect("second complete generation")
        .generation_handle();
    let first_pointer: DurablePublicationPointerV1 = serde_json::from_slice(
        &std::fs::read(first_store_root.join("active-code-generation-v1.json"))
            .expect("first active generation pointer"),
    )
    .expect("decode first active generation pointer");
    let second_pointer: DurablePublicationPointerV1 = serde_json::from_slice(
        &std::fs::read(second_store_root.join("active-code-generation-v1.json"))
            .expect("second active generation pointer"),
    )
    .expect("decode second active generation pointer");

    let identity = profile_identity::load_or_create(&profile_root).expect("profile identity");
    let expected_first_scope = StoreShardIdV1::code(
        identity.brain_id().clone(),
        identity.profile_id().clone(),
        project_id.clone(),
        first_generation.snapshot().repository.clone(),
        CodeShardScopeV1::Worktree {
            worktree_id: first_scheduler.identity().worktree_id().clone(),
        },
    );
    let expected_second_scope = StoreShardIdV1::code(
        identity.brain_id().clone(),
        identity.profile_id().clone(),
        project_id.clone(),
        second_generation.snapshot().repository.clone(),
        CodeShardScopeV1::Worktree {
            worktree_id: second_scheduler.identity().worktree_id().clone(),
        },
    );
    assert!(first_generation.snapshot().reference.is_some());
    assert!(second_generation.snapshot().reference.is_some());

    let _database_scope = tracedecay_runtime_core::db::enter_daemon_database_scope(
        &profile_root,
        73,
        "semantic linked worktree bindings",
    )
    .expect("daemon database scope");
    let registry = DaemonSessionRuntimeRegistryV1::open(identity)
        .await
        .expect("session runtime registry");
    let project_database = registry
        .project_memory(
            project_id.clone(),
            [first_root.clone(), second_root.clone()],
        )
        .await
        .expect("shared project database");
    let first_runtime = registry
        .retain_code_graph_runtime(
            project_id.clone(),
            first_generation.snapshot().repository.clone(),
            first_scheduler.identity().worktree_id().clone(),
            first_generation.snapshot().reference.clone(),
            first_generation.manifest().generation_id.clone(),
            Arc::clone(&project_database),
            CodeGraphReplayBindingV1 {
                generations_root: first_store_root.join("code-generations-v1"),
                sealed_state_digest: tracedecay_graph_db::SealedGraphStateDigest::try_from(
                    first_pointer.state_digest,
                )
                .expect("first sealed state digest"),
            },
            Some(Arc::clone(&first_generation)),
        )
        .await
        .expect("retain first code graph runtime");
    let second_runtime = registry
        .retain_code_graph_runtime(
            project_id,
            second_generation.snapshot().repository.clone(),
            second_scheduler.identity().worktree_id().clone(),
            second_generation.snapshot().reference.clone(),
            second_generation.manifest().generation_id.clone(),
            Arc::clone(&project_database),
            CodeGraphReplayBindingV1 {
                generations_root: second_store_root.join("code-generations-v1"),
                sealed_state_digest: tracedecay_graph_db::SealedGraphStateDigest::try_from(
                    second_pointer.state_digest,
                )
                .expect("second sealed state digest"),
            },
            Some(Arc::clone(&second_generation)),
        )
        .await
        .expect("retain second code graph runtime");
    let first_source_scope = first_runtime.semantic_vector_staging_binding().0.clone();
    let second_source_scope = second_runtime.semantic_vector_staging_binding().0.clone();
    let first_source_dependency = first_runtime
        .semantic_vector_identity()
        .expect("first semantic identity")
        .4;
    let second_source_dependency = second_runtime
        .semantic_vector_identity()
        .expect("second semantic identity")
        .4;
    assert_eq!(first_source_scope, expected_first_scope);
    assert_eq!(second_source_scope, expected_second_scope);
    assert_ne!(first_source_scope, second_source_scope);
    assert_ne!(first_source_dependency, second_source_dependency);

    first_runtime
        .publish_verified_snapshot(&first_generation, Arc::new(AtomicBool::new(false)))
        .expect("publish first code graph");
    second_runtime
        .publish_verified_snapshot(&second_generation, Arc::new(AtomicBool::new(false)))
        .expect("publish second code graph");
    let first_retained = retain_semantic_graph(first_runtime, &first_root);
    let second_retained = retain_semantic_graph(second_runtime, &second_root);
    let first_store = GraphVectorGenerationStoreV1::open(&first_retained)
        .await
        .expect("open first semantic vector store");
    let second_store = GraphVectorGenerationStoreV1::open(&second_retained)
        .await
        .expect("open second semantic vector store");
    let (first_plan, first_prepared, first_descriptor) = prepared_generation(
        &first_generation.manifest().generation_id,
        "chunk.linked-first",
        'c',
    );
    let (second_plan, second_prepared, second_descriptor) = prepared_generation(
        &second_generation.manifest().generation_id,
        "chunk.linked-second",
        'd',
    );
    first_store
        .configure_stage(first_descriptor)
        .expect("configure first semantic stage");
    second_store
        .configure_stage(second_descriptor)
        .expect("configure second semantic stage");
    let first_build = first_store
        .begin_generation(first_plan, Arc::new(NeverCancelled))
        .await
        .expect("begin first semantic generation")
        .build_id()
        .clone();
    first_store
        .commit_batch(&first_build, None, first_prepared, Arc::new(NeverCancelled))
        .await
        .expect("commit first semantic generation");
    let first_publication = first_store
        .publish_generation(&first_build, Arc::new(NeverCancelled))
        .await
        .expect("publish first semantic generation");
    let second_build = second_store
        .begin_generation(second_plan, Arc::new(NeverCancelled))
        .await
        .expect("begin second semantic generation")
        .build_id()
        .clone();
    second_store
        .commit_batch(
            &second_build,
            None,
            second_prepared,
            Arc::new(NeverCancelled),
        )
        .await
        .expect("commit second semantic generation");
    let second_publication = second_store
        .publish_generation(&second_build, Arc::new(NeverCancelled))
        .await
        .expect("publish second semantic generation");

    let retention = first_store
        .reserve_one_generation(None, Arc::new(NeverCancelled))
        .await
        .expect("read project vector census");
    let revision = match retention {
        tracedecay_graph_db::SemanticVectorRetentionStep::Census(census) => census.revision,
        tracedecay_graph_db::SemanticVectorRetentionStep::Reserved {
            census,
            reservation,
        } => {
            first_store
                .release_reserved_generation(*reservation)
                .await
                .expect("release census reservation");
            census.revision
        }
    };
    let first_code_scope = tracedecay_store::SemanticVectorCodeScopeHash::new(
        tracedecay_code_index_retention::code_index_generations::code_index_scope_hash(&first_root),
    )
    .expect("first code scope hash");
    let second_code_scope = tracedecay_store::SemanticVectorCodeScopeHash::new(
        tracedecay_code_index_retention::code_index_generations::code_index_scope_hash(
            &second_root,
        ),
    )
    .expect("second code scope hash");
    assert_eq!(
        first_store
            .source_scope_binding(&first_code_scope, revision, Arc::new(NeverCancelled),)
            .await
            .expect("first durable source binding"),
        SemanticVectorSourceScopeBindingLookup::Exact(first_source_scope.clone())
    );
    assert_eq!(
        first_store
            .source_scope_binding(&second_code_scope, revision, Arc::new(NeverCancelled),)
            .await
            .expect("second durable source binding"),
        SemanticVectorSourceScopeBindingLookup::Exact(second_source_scope.clone())
    );
    let first_dependency = match first_store
        .published_generation_dependency(
            &first_publication.generation_id,
            revision,
            Arc::new(NeverCancelled),
        )
        .await
        .expect("first publication dependency")
    {
        SemanticVectorPublishedGenerationDependencyLookup::Published(dependency) => dependency,
        SemanticVectorPublishedGenerationDependencyLookup::Missing => {
            panic!("first publication dependency is missing")
        }
    };
    let second_dependency = match first_store
        .published_generation_dependency(
            &second_publication.generation_id,
            revision,
            Arc::new(NeverCancelled),
        )
        .await
        .expect("second publication dependency")
    {
        SemanticVectorPublishedGenerationDependencyLookup::Published(dependency) => dependency,
        SemanticVectorPublishedGenerationDependencyLookup::Missing => {
            panic!("second publication dependency is missing")
        }
    };
    assert_eq!(first_dependency.source_scope, first_source_scope);
    assert_eq!(first_dependency.code_scope_hash, first_code_scope);
    assert_eq!(
        first_dependency.source_generation.as_str(),
        first_generation.manifest().generation_id.as_str()
    );
    assert_eq!(
        first_dependency
            .source_dependency
            .generation
            .generation
            .as_str(),
        first_source_dependency.generation.as_str()
    );
    assert_eq!(
        first_dependency.source_dependency.idempotency_key.as_str(),
        first_source_dependency.idempotency_key.as_str()
    );
    assert_eq!(second_dependency.source_scope, second_source_scope);
    assert_eq!(second_dependency.code_scope_hash, second_code_scope);
    assert_eq!(
        second_dependency.source_generation.as_str(),
        second_generation.manifest().generation_id.as_str()
    );
    assert_eq!(
        second_dependency
            .source_dependency
            .generation
            .generation
            .as_str(),
        second_source_dependency.generation.as_str()
    );
    assert_eq!(
        second_dependency.source_dependency.idempotency_key.as_str(),
        second_source_dependency.idempotency_key.as_str()
    );
}
