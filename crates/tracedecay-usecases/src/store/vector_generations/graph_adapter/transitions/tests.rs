use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::time::{Duration, Instant};

use tracedecay_domain::{
    BrainId, ChangedCodeChunkSetV1, ChangedCodeChunkV1, ChunkerRevision, CodeGenerationId,
    CodeSearchChunkId, ContentDigest, EmbeddingDeviceClassV1, EmbeddingDocumentCompositionV1,
    EmbeddingMetricV1, EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1,
    EmbeddingProjectionKeyV1, EmbeddingTruncationSideV1, PrivacyDomainId, ProjectId,
    ProjectionBatchRequestV1, ProjectionOperationV1, ProjectionOutcomeV1, ProjectionReplayReasonV1,
    RepositoryId, UserProfileId, VectorGenerationIdV1, WorktreeId, canonical_sha256,
};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphGenerationDependency, GraphGenerationId,
    GraphIdempotencyKey, GraphNamespace, GraphProjectionId, GraphProjectionIdentity,
    GraphWriteBatch, NeverCancelled, VerifiedGenerationBatchCommit, VerifiedGenerationBeginV1,
    VerifiedGraphSnapshot,
};
use tracedecay_semantic::projector::{ProjectedChunkVectorV1, vector_output_digest};
use tracedecay_store::{
    CodeShardScopeV1, GraphGenerationIdV1, GraphNamespaceV1, GraphProjectionIdV1,
    GraphProjectionIdentityV1, GraphPublicationIdempotencyKeyV1, GraphPublicationKeyV1,
    GraphVerifiedHeadV1, SemanticVectorBuildId, SemanticVectorPublishedGenerationKey,
    SemanticVectorPublishedGenerationLookup, SemanticVectorReconstructionRecipe,
    SemanticVectorSourceGenerationId, SemanticVectorStageBatchReceipt,
    SemanticVectorStageCancelOutcome, SemanticVectorStageKey, SemanticVectorStagePlan,
    SemanticVectorStagePublicationPrepareOutcome, SemanticVectorStagePublishOutcome,
    SemanticVectorStagePublishSettlement, SemanticVectorStageResumeOutcome,
    SemanticVectorWriterFence, StoreRuntimeBindingV1, StoreShardIdV1,
    semantic_vector_chunk_manifest_digest,
};

use super::{post_commit_publication_settlement_error, semantic_stage_source_identity};
use crate::semantic_runtime::{
    RetainedSemanticVectorGraphV1, SemanticGraphExecutionAuthorityV1, SemanticVectorGraphScopeV1,
    SemanticVectorRetentionAuthorizationV1, VerifiedSemanticVectorGraphRuntimeV1,
};
use crate::store::vector_generations::graph_adapter::evaluation_runtime::IsolatedSemanticEvaluationGraphV1;
use crate::store::vector_generations::graph_adapter::{
    GRAPH_BACKGROUND_OPERATION_BUDGET, GRAPH_OPERATION_DEADLINE,
};
use crate::store::vector_generations::{
    GraphVectorGenerationStoreV1, PreparedVectorGenerationV1, SemanticVectorStageDescriptorV1,
    VectorGenerationBeginOutcomeV1, VectorGenerationBuildIdV1, VectorGenerationPlanV1,
    VectorGenerationStoreErrorV1,
};

#[test]
fn published_stage_settlement_interrupt_is_replayable_durability_uncertainty() {
    for interruption in [GraphDbError::Cancelled, GraphDbError::DeadlineExceeded] {
        assert!(matches!(
            post_commit_publication_settlement_error(interruption),
            VectorGenerationStoreErrorV1::DurabilityUncertain(ref message)
                if message.contains("settlement replays on the next publish drive")
        ));
    }
}

#[test]
fn semantic_plan_keeps_code_scope_and_projects_dependency_through_project_shard() {
    let code_shard = StoreShardIdV1::code(
        BrainId::new("brain.semantic-plan").unwrap(),
        UserProfileId::new("profile.semantic-plan").unwrap(),
        ProjectId::new("project.semantic-plan").unwrap(),
        RepositoryId::new("repository.semantic-plan").unwrap(),
        CodeShardScopeV1::Worktree {
            worktree_id: WorktreeId::new("worktree.semantic-plan").unwrap(),
        },
    );
    let project_shard = StoreShardIdV1::project(
        BrainId::new("brain.semantic-plan").unwrap(),
        UserProfileId::new("profile.semantic-plan").unwrap(),
        ProjectId::new("project.semantic-plan").unwrap(),
    );
    let binding: StoreRuntimeBindingV1 = serde_json::from_value(serde_json::json!({
        "shard_id": project_shard,
        "incarnation": 7,
        "authority_epoch": 11
    }))
    .unwrap();
    let dependency = GraphGenerationDependency::new(
        GraphProjectionIdentity::new(
            GraphNamespace::new("code.source").unwrap(),
            GraphProjectionId::new("code.projection").unwrap(),
        ),
        GraphGenerationId::new("code.generation").unwrap(),
        GraphIdempotencyKey::new("code.publication").unwrap(),
    );
    let (source_scope, source_dependency) =
        semantic_stage_source_identity(&code_shard, &binding, &dependency).unwrap();
    let projection = GraphProjectionIdentityV1 {
        shard_id: binding.shard_id.clone(),
        namespace: GraphNamespaceV1::new("semantic.vector").unwrap(),
        projection: GraphProjectionIdV1::new("chunks").unwrap(),
    };
    let plan = SemanticVectorStagePlan::new(
        projection.clone(),
        SemanticVectorBuildId::new("build.semantic-plan").unwrap(),
        VectorGenerationIdV1::new(canonical_sha256(&"semantic-plan-generation").unwrap()),
        None,
        GraphPublicationKeyV1::new(
            projection.clone(),
            GraphGenerationIdV1::new("generation.semantic-plan").unwrap(),
            GraphPublicationIdempotencyKeyV1::new("publication.semantic-plan").unwrap(),
        ),
        source_scope.clone(),
        tracedecay_store::SemanticVectorCodeScopeHash::new("a".repeat(64)).unwrap(),
        SemanticVectorSourceGenerationId::new("source.semantic-plan").unwrap(),
        source_dependency.clone(),
        SemanticVectorReconstructionRecipe {
            source_manifest_digest: digest('1'),
            embedding_projection_digest: digest('2'),
            embedding_dimension: 3,
            model_artifact_digest: digest('3'),
            projection_manifest_digest: digest('4'),
            privacy_domain_digest: digest('5'),
            privacy_key_epoch: 1,
            expected_chunk_manifest_digest: semantic_vector_chunk_manifest_digest(&[]).unwrap(),
        },
        0,
        None,
        digest('9'),
        SemanticVectorWriterFence {
            binding: binding.clone(),
        },
    )
    .unwrap();

    plan.validate().unwrap();
    assert_eq!(plan.source_scope, code_shard);
    assert_eq!(
        plan.source_dependency.generation.projection.shard_id,
        binding.shard_id
    );
    assert_ne!(
        plan.source_dependency.generation.projection.shard_id,
        plan.source_scope
    );
}

#[tokio::test]
async fn same_binding_store_preserves_an_active_pending_stage() {
    let first_source = CodeGenerationId::new("code-generation.superseded").unwrap();
    let second_source = CodeGenerationId::new("code-generation.current").unwrap();
    let cancellation: Arc<dyn GraphCancellation> = Arc::new(NeverCancelled);
    let graph = Arc::new(
        IsolatedSemanticEvaluationGraphV1::open_source_generations(
            &[first_source.clone(), second_source.clone()],
            Arc::clone(&cancellation),
        )
        .unwrap(),
    );
    let embedding = admitted_embedding();
    let (first_plan, first_prepared, first_descriptor) =
        prepared_generation(&first_source, "chunk.superseded", 'a', &embedding);
    let first_retained = graph.retained(&first_source).unwrap();
    let first_store = GraphVectorGenerationStoreV1::open(&first_retained).unwrap();
    first_store.configure_stage(first_descriptor).unwrap();
    let first_build = match first_store
        .begin_generation(first_plan, Arc::clone(&cancellation))
        .await
        .unwrap()
    {
        VectorGenerationBeginOutcomeV1::ReplayFromStart { build_id } => build_id,
        VectorGenerationBeginOutcomeV1::AlreadyPublished { .. } => {
            panic!("first source generation must begin a pending stage")
        }
    };
    first_store
        .commit_batch(
            &first_build,
            None,
            first_prepared,
            Arc::clone(&cancellation),
        )
        .await
        .unwrap();
    let first_stage = first_store
        .pending
        .lock()
        .unwrap()
        .get(&first_build)
        .unwrap()
        .stage
        .plan
        .key
        .clone();
    drop(first_store);

    let (second_plan, _second_prepared, second_descriptor) =
        prepared_generation(&second_source, "chunk.current", 'b', &embedding);
    let second_retained = graph.retained(&second_source).unwrap();
    let second_store = GraphVectorGenerationStoreV1::open(&second_retained).unwrap();
    second_store.configure_stage(second_descriptor).unwrap();
    let error = second_store
        .begin_generation(second_plan, Arc::clone(&cancellation))
        .await
        .expect_err("a pending stage owned by this binding is active concurrency");
    assert!(matches!(
        error,
        VectorGenerationStoreErrorV1::ConcurrentMutation(ref context)
            if context.site == "usecases.store.begin_generation.occupied_stage_active_writer"
    ));
    let authority = SemanticGraphExecutionAuthorityV1::new(
        Arc::clone(&cancellation),
        std::time::Instant::now() + std::time::Duration::from_secs(30),
    );
    assert!(matches!(
        first_retained
            .runtime()
            .resume_stage(&first_stage, &authority)
            .unwrap(),
        tracedecay_store::SemanticVectorStageResumeOutcome::Pending(record)
            if record.plan.key == first_stage
    ));
}

/// One committed generation, ready to publish.
async fn staged_publication(
    label: &str,
    byte: char,
    cancellation: &Arc<dyn GraphCancellation>,
) -> (
    Arc<IsolatedSemanticEvaluationGraphV1>,
    GraphVectorGenerationStoreV1,
    VectorGenerationBuildIdV1,
    CodeGenerationId,
) {
    let source = CodeGenerationId::new(format!("code-generation.{label}")).unwrap();
    let graph = Arc::new(
        IsolatedSemanticEvaluationGraphV1::open_source_generations(
            std::slice::from_ref(&source),
            Arc::clone(cancellation),
        )
        .unwrap(),
    );
    let retained = graph.retained(&source).unwrap();
    let store = GraphVectorGenerationStoreV1::open(&retained).unwrap();
    let (plan, prepared, descriptor) = prepared_generation(
        &source,
        &format!("chunk.{label}"),
        byte,
        &admitted_embedding(),
    );
    store.configure_stage(descriptor).unwrap();
    let build = store
        .begin_generation(plan, Arc::clone(cancellation))
        .await
        .unwrap()
        .build_id()
        .clone();
    store
        .commit_batch(&build, None, prepared, Arc::clone(cancellation))
        .await
        .unwrap();
    (graph, store, build, source)
}

#[tokio::test]
async fn corpus_scaled_publication_uses_fresh_background_authority_per_phase() {
    let cancellation: Arc<dyn GraphCancellation> = Arc::new(NeverCancelled);
    let (_graph, mut store, build, source) =
        staged_publication("background-publication", 'c', &cancellation).await;
    let probe = Arc::new(PublicationAuthorityProbeRuntime::wrapping(Arc::clone(
        &store.runtime,
    )));
    store.replace_runtime(probe);

    let publication = store
        .publish_generation(&build, cancellation)
        .await
        .unwrap();

    assert_eq!(publication.checkpoint.source_generation, source);
}

/// #837: the whole-generation digest over 168k chunks alone outran the 30s
/// interactive deadline, and the install phase inherited what was left of the
/// same authority — nothing. Each phase now mints its own background
/// authority, so a verify pass that is itself longer than the interactive
/// deadline still leaves the install a full budget.
#[tokio::test]
async fn verify_pass_outlasting_the_interactive_deadline_still_publishes() {
    let cancellation: Arc<dyn GraphCancellation> = Arc::new(NeverCancelled);
    let (_graph, mut store, build, source) =
        staged_publication("long-verify", 'f', &cancellation).await;
    let probe = Arc::new(PublicationAuthorityProbeRuntime {
        // Twice the interactive deadline: enough that a shared 30s authority
        // is already spent before the install phase starts.
        simulated_verify_elapsed: Some(2 * GRAPH_OPERATION_DEADLINE),
        ..PublicationAuthorityProbeRuntime::wrapping(Arc::clone(&store.runtime))
    });
    store.replace_runtime(Arc::clone(&probe) as Arc<dyn VerifiedSemanticVectorGraphRuntimeV1>);

    let publication = store
        .publish_generation(&build, cancellation)
        .await
        .unwrap();

    assert_eq!(publication.checkpoint.source_generation, source);
    assert_eq!(probe.publish_calls.load(Ordering::SeqCst), 1);
}

/// Lifecycle cancellation during the verify pass ends the publication with a
/// typed `Cancelled`, and the install phase never starts. The background
/// budget replaces the wall clock, not cancellation.
#[tokio::test]
async fn publication_cancelled_during_verify_reports_typed_cancellation() {
    let cancellation_flag = Arc::new(AtomicBool::new(false));
    let cancellation: Arc<dyn GraphCancellation> =
        Arc::new(SwitchCancellation(Arc::clone(&cancellation_flag)));
    let (_graph, mut store, build, _source) =
        staged_publication("cancelled-verify", 'b', &cancellation).await;
    let probe = Arc::new(PublicationAuthorityProbeRuntime {
        cancel_during_verify: Some(cancellation_flag),
        ..PublicationAuthorityProbeRuntime::wrapping(Arc::clone(&store.runtime))
    });
    store.replace_runtime(Arc::clone(&probe) as Arc<dyn VerifiedSemanticVectorGraphRuntimeV1>);

    assert!(matches!(
        store.publish_generation(&build, cancellation).await,
        Err(VectorGenerationStoreErrorV1::Cancelled)
    ));
    assert_eq!(probe.publish_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn corpus_scaled_generation_begin_uses_background_authority() {
    let source = CodeGenerationId::new("code-generation.background-begin").unwrap();
    let cancellation: Arc<dyn GraphCancellation> = Arc::new(NeverCancelled);
    let graph = Arc::new(
        IsolatedSemanticEvaluationGraphV1::open_source_generations(
            std::slice::from_ref(&source),
            Arc::clone(&cancellation),
        )
        .unwrap(),
    );
    let retained = graph.retained(&source).unwrap();
    let mut store = GraphVectorGenerationStoreV1::open(&retained).unwrap();
    let (plan, _, descriptor) = prepared_generation(
        &source,
        "chunk.background-begin",
        'd',
        &admitted_embedding(),
    );
    store.configure_stage(descriptor).unwrap();
    let probe = Arc::new(PublicationAuthorityProbeRuntime {
        require_background_begin: true,
        ..PublicationAuthorityProbeRuntime::wrapping(Arc::clone(&store.runtime))
    });
    store.replace_runtime(probe);

    let outcome = store.begin_generation(plan, cancellation).await.unwrap();

    assert!(matches!(
        outcome,
        VectorGenerationBeginOutcomeV1::ReplayFromStart { .. }
    ));
    assert_eq!(
        GRAPH_BACKGROUND_OPERATION_BUDGET,
        Duration::from_secs(15 * 60),
        "generation restart must have a finite corpus-scale authority"
    );
}

struct SwitchCancellation(Arc<AtomicBool>);

impl GraphCancellation for SwitchCancellation {
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[tokio::test]
async fn generation_begin_releases_on_lifecycle_cancellation_during_snapshot_refresh() {
    let source = CodeGenerationId::new("code-generation.begin-cancelled").unwrap();
    let cancellation_flag = Arc::new(AtomicBool::new(false));
    let cancellation: Arc<dyn GraphCancellation> =
        Arc::new(SwitchCancellation(Arc::clone(&cancellation_flag)));
    let graph = Arc::new(
        IsolatedSemanticEvaluationGraphV1::open_source_generations(
            std::slice::from_ref(&source),
            Arc::clone(&cancellation),
        )
        .unwrap(),
    );
    let retained = graph.retained(&source).unwrap();
    let mut store = GraphVectorGenerationStoreV1::open(&retained).unwrap();
    let (plan, _, descriptor) =
        prepared_generation(&source, "chunk.begin-cancelled", 'e', &admitted_embedding());
    store.configure_stage(descriptor).unwrap();
    let probe = Arc::new(PublicationAuthorityProbeRuntime {
        cancellation_to_trip: Some(cancellation_flag),
        ..PublicationAuthorityProbeRuntime::wrapping(Arc::clone(&store.runtime))
    });
    store.replace_runtime(probe);

    assert!(matches!(
        store.begin_generation(plan, cancellation).await,
        Err(VectorGenerationStoreErrorV1::Cancelled)
    ));
}

#[tokio::test]
async fn historical_mapping_still_requires_a_live_generation_lease_after_lookup() {
    let source = CodeGenerationId::new("code-generation.retired-after-lookup").unwrap();
    let cancellation: Arc<dyn GraphCancellation> = Arc::new(NeverCancelled);
    let graph = Arc::new(
        IsolatedSemanticEvaluationGraphV1::open_source_generations(
            std::slice::from_ref(&source),
            Arc::clone(&cancellation),
        )
        .unwrap(),
    );
    let retained = graph.retained(&source).unwrap();
    let store = GraphVectorGenerationStoreV1::open(&retained).unwrap();
    let (plan, prepared, descriptor) = prepared_generation(
        &source,
        "chunk.retired-after-lookup",
        'f',
        &admitted_embedding(),
    );
    store.configure_stage(descriptor).unwrap();
    let build = store
        .begin_generation(plan, Arc::clone(&cancellation))
        .await
        .unwrap()
        .build_id()
        .clone();
    store
        .commit_batch(&build, None, prepared, Arc::clone(&cancellation))
        .await
        .unwrap();
    let publication = store
        .publish_generation(&build, Arc::clone(&cancellation))
        .await
        .unwrap();
    let retired = Arc::new(AtomicBool::new(false));
    let probe = Arc::new(PublicationAuthorityProbeRuntime {
        retire_after_published_lookup: Some(Arc::clone(&retired)),
        ..PublicationAuthorityProbeRuntime::wrapping(Arc::clone(retained.runtime()))
    });
    let historical = RetainedSemanticVectorGraphV1::new(probe, cancellation);

    let error = match GraphVectorGenerationStoreV1::read_only_generation(
        &historical,
        &publication.generation_id,
    ) {
        Err(error) => error,
        Ok(_) => panic!("a retired graph generation must not be served from stale mapping"),
    };
    assert!(retired.load(Ordering::SeqCst));
    assert!(matches!(
        error,
        VectorGenerationStoreErrorV1::Unavailable(ref message)
            if message == "published graph generation was retired after its mapping read"
    ));
}

/// Stands in for `EXACT_SQL_TRANSACTION_IDLE_LIMIT`, shrunk so the test fails
/// in seconds instead of parking for the real abandoned-transaction lease.
const WRITER_GATE_LEASE: Duration = Duration::from_secs(2);

/// The store's durable operations acquire the project's one exclusive writer
/// synchronously. Running that on the runtime is what starved the tasks that
/// would commit the transaction it waits for, so the writer stayed occupied
/// until its idle lease expired. On a single-worker runtime the contender must
/// leave that worker free: a heartbeat keeps ticking and the transaction holder
/// still gets polled to release the writer, well inside the idle limit.
#[tokio::test(flavor = "current_thread")]
async fn writer_contention_leaves_the_only_runtime_worker_free_to_commit() {
    let source = CodeGenerationId::new("code-generation.writer-contention").unwrap();
    let cancellation: Arc<dyn GraphCancellation> = Arc::new(NeverCancelled);
    let graph = Arc::new(
        IsolatedSemanticEvaluationGraphV1::open_source_generations(
            std::slice::from_ref(&source),
            Arc::clone(&cancellation),
        )
        .unwrap(),
    );
    let retained = graph.retained(&source).unwrap();
    let mut store = GraphVectorGenerationStoreV1::open(&retained).unwrap();
    let (plan, _, descriptor) = prepared_generation(
        &source,
        "chunk.writer-contention",
        'f',
        &admitted_embedding(),
    );
    store.configure_stage(descriptor).unwrap();
    let (release, gate) = std::sync::mpsc::channel();
    let probe = Arc::new(PublicationAuthorityProbeRuntime {
        begin_gate: Mutex::new(Some(gate)),
        ..PublicationAuthorityProbeRuntime::wrapping(Arc::clone(&store.runtime))
    });
    store.replace_runtime(probe);

    let ticks = Arc::new(AtomicUsize::new(0));
    let heartbeat_ticks = Arc::clone(&ticks);
    let heartbeat = tokio::spawn(async move {
        loop {
            tokio::time::sleep(Duration::from_millis(5)).await;
            heartbeat_ticks.fetch_add(1, Ordering::SeqCst);
        }
    });
    let holder = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        release.send(()).is_ok()
    });

    let started = Instant::now();
    let outcome = store.begin_generation(plan, cancellation).await;
    let elapsed = started.elapsed();
    heartbeat.abort();

    assert!(
        matches!(
            outcome,
            Ok(VectorGenerationBeginOutcomeV1::ReplayFromStart { .. })
        ),
        "the writer must be released by the holder, never by the lease expiry: {outcome:?}"
    );
    assert!(
        holder.await.unwrap(),
        "the transaction holder must still be polled while the writer is contended"
    );
    assert!(
        ticks.load(Ordering::SeqCst) > 0,
        "the single runtime worker must keep running other tasks while the store waits"
    );
    assert!(
        elapsed < WRITER_GATE_LEASE,
        "begin must not have waited out the lease: {elapsed:?}"
    );
}

struct PublicationAuthorityProbeRuntime {
    inner: Arc<dyn VerifiedSemanticVectorGraphRuntimeV1>,
    require_background_begin: bool,
    prepare_deadline: Mutex<Option<Instant>>,
    cancellation_to_trip: Option<Arc<AtomicBool>>,
    /// Stands in for the project's exclusive exact-SQL writer: `begin_stage`
    /// blocks here until the transaction holder releases it.
    begin_gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    /// How long the whole-generation digest pass would run at corpus scale.
    /// Simulated rather than slept: what the fix has to survive is the
    /// *elapsed budget*, and a real 168k-chunk pass is not a unit test.
    simulated_verify_elapsed: Option<Duration>,
    /// When that simulated verify pass would have finished.
    verify_completed_at: Mutex<Option<Instant>>,
    /// Trips lifecycle cancellation from inside the verify pass.
    cancel_during_verify: Option<Arc<AtomicBool>>,
    publish_calls: AtomicUsize,
    retire_after_published_lookup: Option<Arc<AtomicBool>>,
}

impl PublicationAuthorityProbeRuntime {
    fn wrapping(inner: Arc<dyn VerifiedSemanticVectorGraphRuntimeV1>) -> Self {
        Self {
            inner,
            require_background_begin: false,
            prepare_deadline: Mutex::new(None),
            cancellation_to_trip: None,
            begin_gate: Mutex::new(None),
            simulated_verify_elapsed: None,
            verify_completed_at: Mutex::new(None),
            cancel_during_verify: None,
            publish_calls: AtomicUsize::new(0),
            retire_after_published_lookup: None,
        }
    }
}

impl VerifiedSemanticVectorGraphRuntimeV1 for PublicationAuthorityProbeRuntime {
    fn scope(&self) -> &SemanticVectorGraphScopeV1 {
        self.inner.scope()
    }

    fn recover_verified_snapshot(
        &self,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
        if let Some(cancellation) = &self.cancellation_to_trip {
            cancellation.store(true, Ordering::SeqCst);
            authority.checkpoint()?;
        }
        self.inner.recover_verified_snapshot(authority)
    }

    fn recover_verified_generation(
        &self,
        publication: &GraphPublicationKeyV1,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        if self
            .retire_after_published_lookup
            .as_ref()
            .is_some_and(|retired| retired.load(Ordering::SeqCst))
        {
            return Err(GraphDbError::unavailable(
                "published graph generation was retired after its mapping read",
            ));
        }
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
        if let Some(gate) = self.begin_gate.lock().unwrap().take() {
            // Bounded exactly as the real writer wait is bounded: by the
            // abandoned-transaction lease. Expiring here means the holder was
            // never polled to release, which is the starvation this guards.
            gate.recv_timeout(WRITER_GATE_LEASE)
                .map_err(|_| GraphDbError::Cancelled)?;
        }
        if self.require_background_begin {
            let remaining = authority
                .deadline()
                .checked_duration_since(Instant::now())
                .ok_or(GraphDbError::DeadlineExceeded)?;
            if !(Duration::from_secs(14 * 60)..=GRAPH_BACKGROUND_OPERATION_BUDGET)
                .contains(&remaining)
            {
                return Err(GraphDbError::DeadlineExceeded);
            }
        }
        self.inner.begin_stage(plan, authority)
    }

    fn resume_stage(
        &self,
        stage: &SemanticVectorStageKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorStageResumeOutcome, GraphDbError> {
        self.inner.resume_stage(stage, authority)
    }

    fn published_semantic_generation(
        &self,
        key: &SemanticVectorPublishedGenerationKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<SemanticVectorPublishedGenerationLookup, GraphDbError> {
        let lookup = self.inner.published_semantic_generation(key, authority)?;
        if matches!(
            &lookup,
            SemanticVectorPublishedGenerationLookup::Published { .. }
        ) && let Some(retired) = &self.retire_after_published_lookup
        {
            retired.store(true, Ordering::SeqCst);
        }
        Ok(lookup)
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
        let remaining = authority
            .deadline()
            .checked_duration_since(Instant::now())
            .ok_or(GraphDbError::DeadlineExceeded)?;
        if !(Duration::from_secs(14 * 60)..=GRAPH_BACKGROUND_OPERATION_BUDGET).contains(&remaining)
        {
            return Err(GraphDbError::DeadlineExceeded);
        }
        *self.prepare_deadline.lock().unwrap() = Some(authority.deadline());
        if let Some(cancellation) = &self.cancel_during_verify {
            cancellation.store(true, Ordering::SeqCst);
            authority.checkpoint()?;
        }
        if let Some(elapsed) = self.simulated_verify_elapsed {
            // The whole-generation digest pass, charged against this phase's
            // own authority. Outliving the interactive deadline here is the
            // corpus-scale case; not outliving the phase budget is the fix.
            let completed_at = Instant::now() + elapsed;
            if authority.deadline() <= completed_at {
                return Err(GraphDbError::DeadlineExceeded);
            }
            *self.verify_completed_at.lock().unwrap() = Some(completed_at);
        }
        let outcome = self
            .inner
            .prepare_publication_from_staged_native(stage, authority)?;
        Ok(outcome)
    }

    fn publish_ready_stage(
        &self,
        stage: &SemanticVectorStageKey,
        authority: &SemanticGraphExecutionAuthorityV1,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        self.publish_calls.fetch_add(1, Ordering::SeqCst);
        let prepare_deadline = self
            .prepare_deadline
            .lock()
            .unwrap()
            .ok_or_else(|| GraphDbError::conflict("test.publish_without_prepare"))?;
        if authority.deadline() <= prepare_deadline {
            return Err(GraphDbError::DeadlineExceeded);
        }
        // The install has to start after the simulated verify pass finished and
        // still hold budget. One authority shared across both phases cannot.
        if let Some(completed_at) = *self.verify_completed_at.lock().unwrap()
            && authority.deadline() <= completed_at
        {
            return Err(GraphDbError::DeadlineExceeded);
        }
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
        generation: &VectorGenerationIdV1,
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

fn admitted_embedding() -> tracedecay_domain::AdmittedEmbeddingProjectionKeyV1 {
    EmbeddingProjectionKeyV1 {
        model_artifact_digest: digest('1'),
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
        runtime_backend: "fixture-runtime".to_owned(),
        runtime_build_revision: "fixture-runtime.v1".to_owned(),
        device_class: EmbeddingDeviceClassV1::Cpu,
        dimensions: 1,
        metric: EmbeddingMetricV1::Cosine,
        normalization: EmbeddingNormalizationV1::L2,
        precision: EmbeddingPrecisionV1::Fp32,
        chunk_schema_revision: "code-search-chunk.v1".to_owned(),
        chunker_revision: ChunkerRevision::new("chunker.fixture.v1").unwrap(),
        privacy_domain: PrivacyDomainId::new("privacy.fixture").unwrap(),
        privacy_key_epoch: 1,
    }
    .admit()
    .unwrap()
}

fn prepared_generation(
    source: &tracedecay_domain::CodeGenerationId,
    chunk: &str,
    digest_byte: char,
    embedding: &tracedecay_domain::AdmittedEmbeddingProjectionKeyV1,
) -> (
    VectorGenerationPlanV1,
    PreparedVectorGenerationV1,
    SemanticVectorStageDescriptorV1,
) {
    let chunk_id = CodeSearchChunkId::new(chunk).unwrap();
    let chunk_digest =
        ContentDigest::new(format!("sha256:{}", digest_byte.to_string().repeat(64))).unwrap();
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
    changes.manifest_digest = changes.compute_digest().unwrap();
    let descriptor =
        SemanticVectorStageDescriptorV1::from_changes(embedding.clone(), &changes).unwrap();
    let mut request = ProjectionBatchRequestV1 {
        request_digest: digest('0'),
        changes,
        previous_projection_key: None,
        target_projection_key: embedding.projection_key().clone(),
        replay_reason: ProjectionReplayReasonV1::FullRebuildIncompatible,
    };
    request.request_digest =
        tracedecay_code_index::projection::expected_request_digest(&request).unwrap();
    let values = vec![f32::from(digest_byte as u8)];
    let output_digest = vector_output_digest(
        embedding.projection_key(),
        &chunk_id,
        &chunk_digest,
        &values,
    )
    .unwrap();
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
    .unwrap();
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
    T::try_from(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}
