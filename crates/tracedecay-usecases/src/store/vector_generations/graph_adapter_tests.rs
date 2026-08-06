use std::mem::size_of;
use std::sync::Arc;
use std::time::Duration;

use tracedecay_domain::{
    ChangedCodeChunkSetV1, ChangedCodeChunkV1, ChunkerRevision, CodeGenerationId,
    CodeSearchChunkId, ContentDigest, EmbeddingDeviceClassV1, EmbeddingMetricV1,
    EmbeddingNormalizationV1, EmbeddingPoolingV1, EmbeddingPrecisionV1, EmbeddingProjectionKeyV1,
    EmbeddingTruncationSideV1, ManifestDigest, PrivacyDomainId, ProjectionBatchRequestV1,
    ProjectionReplayReasonV1,
};
use tracedecay_graph_db::{
    GraphDb, GraphDbOwner, GraphDbRegistration, GraphDbRegistry, GraphDbRegistryConfig, GraphLabel,
    GraphMutation, GraphNamespace, GraphProjectionId, GraphPropertyName, GraphWriteBatch,
    NeverCancelled, SourceGeneration,
};
use tracedecay_semantic::projector::{PreparedVectorGenerationV1, ProjectedChunkVectorV1};
use tracedecay_store::{
    BrainId, ProjectId, RetainedGraphStoreLeaseV1, StoreAuthorityEpochV1, StoreIncarnationV1,
    StoreRuntimeBindingV1, StoreShardIdV1, UserProfileId, VerifiedStoreLocatorV1,
    canonical_store_locator_digest,
};

use super::{
    GraphVectorGenerationStoreV1, SemanticHybridGraphSearchRequestV1,
    SemanticHybridLexicalCandidateV1, SemanticVectorGraphSearchRequestV1, VectorGenerationPlanV1,
    VectorGenerationStoreErrorV1,
};
use crate::semantic_runtime::SemanticRetainedVectorGenerationsV1;

struct Cancelled;

impl tracedecay_graph_db::GraphCancellation for Cancelled {
    fn is_cancelled(&self) -> bool {
        true
    }
}

fn id<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("canonical test identity")
}

fn manifest_digest(byte: char) -> ManifestDigest {
    id(&format!("sha256:{}", byte.to_string().repeat(64)))
}

fn content_digest(byte: char) -> ContentDigest {
    id(&format!("sha256:{}", byte.to_string().repeat(64)))
}

fn memory_graph() -> Arc<GraphDb> {
    GraphDbOwner::memory(Arc::new(NeverCancelled))
        .expect("graph opens")
        .handle()
}

#[derive(Debug)]
struct TestGraphLease {
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
    canonical_path: std::path::PathBuf,
}

impl RetainedGraphStoreLeaseV1 for TestGraphLease {
    fn binding(&self) -> &StoreRuntimeBindingV1 {
        &self.binding
    }

    fn verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.verified_locator
    }

    fn canonical_path(&self) -> &std::path::Path {
        &self.canonical_path
    }
}

fn graph_registration(store_root: &std::path::Path) -> GraphDbRegistration {
    let binding = StoreRuntimeBindingV1::new(
        StoreShardIdV1::project(
            BrainId::try_from("brain-semantic-vector-test".to_owned()).unwrap(),
            UserProfileId::try_from("profile-semantic-vector-test".to_owned()).unwrap(),
            ProjectId::try_from("project-semantic-vector-test".to_owned()).unwrap(),
        ),
        StoreIncarnationV1::new(1).unwrap(),
        StoreAuthorityEpochV1::new(1).unwrap(),
    );
    let canonical_path = store_root.join("graph.grafeo");
    GraphDbRegistration {
        authority_lease: Arc::new(TestGraphLease {
            verified_locator: VerifiedStoreLocatorV1::new(
                binding.shard_id.clone(),
                binding.incarnation,
                canonical_store_locator_digest(&canonical_path).unwrap(),
            ),
            binding,
            canonical_path,
        }),
        cancellation: Arc::new(NeverCancelled),
        lifecycle_cancellation: Arc::new(NeverCancelled),
        deadline: std::time::Instant::now() + Duration::from_secs(30),
    }
}

fn admitted_embedding() -> tracedecay_domain::AdmittedEmbeddingProjectionKeyV1 {
    EmbeddingProjectionKeyV1 {
        model_artifact_digest: manifest_digest('1'),
        tokenizer_digest: manifest_digest('2'),
        config_digest: manifest_digest('3'),
        query_instruction_digest: Some(manifest_digest('4')),
        document_instruction_digest: Some(manifest_digest('5')),
        pooling: EmbeddingPoolingV1::Mean,
        truncation_side: EmbeddingTruncationSideV1::Right,
        truncation_length: 512,
        runtime_backend: "fastembed-ort".to_owned(),
        runtime_build_revision: "ort-test-rev-1".to_owned(),
        device_class: EmbeddingDeviceClassV1::Cpu,
        dimensions: 2,
        metric: EmbeddingMetricV1::Cosine,
        normalization: EmbeddingNormalizationV1::L2,
        precision: EmbeddingPrecisionV1::Fp32,
        chunk_schema_revision: "code-search-chunk.v1".to_owned(),
        chunker_revision: id::<ChunkerRevision>("chunker.v1"),
        privacy_domain: id::<PrivacyDomainId>("privacy.project-a"),
        privacy_key_epoch: 7,
    }
    .admit()
    .expect("admitted embedding fixture")
}

fn added_prepared(
    source: &CodeGenerationId,
    chunk_id: &CodeSearchChunkId,
    chunk_digest: &ContentDigest,
    values: Vec<f32>,
) -> PreparedVectorGenerationV1 {
    let embedding_key = admitted_embedding();
    let projection_key = embedding_key.projection_key().clone();
    let output_digest = tracedecay_semantic::projector::vector_output_digest(
        &projection_key,
        chunk_id,
        chunk_digest,
        &values,
    )
    .expect("canonical vector output digest");
    let mut changes = ChangedCodeChunkSetV1 {
        from_generation: None,
        to_generation: source.clone(),
        manifest_digest: manifest_digest('0'),
        added_or_changed: vec![ChangedCodeChunkV1 {
            chunk_id: chunk_id.clone(),
            prior_digest: None,
            current_digest: Some(chunk_digest.clone()),
        }],
        deleted: vec![],
        reused: vec![],
    };
    changes.manifest_digest = changes.compute_digest().expect("changed-set digest");
    let source_manifest_digest = changes.manifest_digest.clone();
    let mut request = ProjectionBatchRequestV1 {
        request_digest: manifest_digest('0'),
        changes,
        previous_projection_key: None,
        target_projection_key: projection_key.clone(),
        replay_reason: ProjectionReplayReasonV1::SourceEdit,
    };
    request.request_digest = tracedecay_code_index::projection::expected_request_digest(&request)
        .expect("request digest");
    let receipt = tracedecay_code_index::projection::build_batch_receipt(
        &request,
        &[
            tracedecay_code_index::projection::ChunkProjectionDecisionV1 {
                chunk_id: chunk_id.clone(),
                prior_chunk_digest: None,
                current_chunk_digest: Some(chunk_digest.clone()),
                operation: tracedecay_domain::ProjectionOperationV1::Added,
                outcome: tracedecay_domain::ProjectionOutcomeV1::Applied,
                output_digest: Some(output_digest.clone()),
            },
        ],
    )
    .expect("receipt");
    PreparedVectorGenerationV1 {
        embedding_key,
        request,
        receipt,
        vectors: vec![ProjectedChunkVectorV1 {
            projection_key,
            source_generation: source.clone(),
            source_manifest_digest,
            chunk_id: chunk_id.clone(),
            chunk_digest: chunk_digest.clone(),
            values,
            output_digest,
        }],
        tombstones: vec![],
    }
}

async fn publish_one(
    store: &GraphVectorGenerationStoreV1,
    source: &str,
    chunk: &str,
    digest: char,
    values: Vec<f32>,
    expected_active: Option<&tracedecay_domain::VectorGenerationIdV1>,
) -> tracedecay_domain::VectorGenerationIdV1 {
    let source = id::<CodeGenerationId>(source);
    let chunk_id = id::<CodeSearchChunkId>(chunk);
    let prepared = added_prepared(&source, &chunk_id, &content_digest(digest), values);
    let plan = VectorGenerationPlanV1 {
        target_projection_key: prepared.embedding_key.projection_key().clone(),
        source_generation: source,
        source_manifest_digest: prepared.request.changes.manifest_digest.clone(),
        expected_chunk_ids: vec![chunk_id].into(),
        base_generation: None,
    };
    let build = store
        .begin_generation(plan, Arc::new(NeverCancelled))
        .await
        .expect("begin");
    store
        .commit_batch(&build, None, prepared, Arc::new(NeverCancelled))
        .await
        .expect("commit");
    store
        .publish_generation(&build, expected_active, Arc::new(NeverCancelled))
        .await
        .expect("publish")
        .generation_id
}

#[tokio::test]
async fn staged_replacement_keeps_previous_generation_readable_until_atomic_publication() {
    let store =
        GraphVectorGenerationStoreV1::open(memory_graph(), Arc::new(NeverCancelled)).unwrap();
    let first = publish_one(
        &store,
        "code-generation.first",
        "chunk.first",
        'a',
        vec![1.0, 0.0],
        None,
    )
    .await;

    let source = id::<CodeGenerationId>("code-generation.second");
    let chunk_id = id::<CodeSearchChunkId>("chunk.second");
    let prepared = added_prepared(&source, &chunk_id, &content_digest('b'), vec![0.0, 1.0]);
    let build = store
        .begin_generation(
            VectorGenerationPlanV1 {
                target_projection_key: prepared.embedding_key.projection_key().clone(),
                source_generation: source,
                source_manifest_digest: prepared.request.changes.manifest_digest.clone(),
                expected_chunk_ids: vec![chunk_id].into(),
                base_generation: None,
            },
            Arc::new(NeverCancelled),
        )
        .await
        .expect("begin replacement");
    store
        .commit_batch(&build, None, prepared, Arc::new(NeverCancelled))
        .await
        .expect("commit replacement");

    assert_eq!(
        store
            .active_generation_id(Arc::new(NeverCancelled))
            .await
            .unwrap(),
        Some(first.clone())
    );
    assert!(
        store
            .generation(&first, Arc::new(NeverCancelled))
            .await
            .unwrap()
            .is_some()
    );

    let second = store
        .publish_generation(&build, Some(&first), Arc::new(NeverCancelled))
        .await
        .expect("publish replacement")
        .generation_id;
    assert_eq!(
        store
            .active_generation_id(Arc::new(NeverCancelled))
            .await
            .unwrap(),
        Some(second)
    );
    assert!(
        store
            .generation(&first, Arc::new(NeverCancelled))
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn cancelled_mutation_leaves_the_active_generation_unchanged() {
    let store =
        GraphVectorGenerationStoreV1::open(memory_graph(), Arc::new(NeverCancelled)).unwrap();
    let active = publish_one(
        &store,
        "code-generation.active",
        "chunk.active",
        'c',
        vec![1.0, 0.0],
        None,
    )
    .await;
    let source = id::<CodeGenerationId>("code-generation.cancelled");
    let chunk_id = id::<CodeSearchChunkId>("chunk.cancelled");
    let prepared = added_prepared(&source, &chunk_id, &content_digest('d'), vec![0.0, 1.0]);
    let error = store
        .begin_generation(
            VectorGenerationPlanV1 {
                target_projection_key: prepared.embedding_key.projection_key().clone(),
                source_generation: source,
                source_manifest_digest: prepared.request.changes.manifest_digest,
                expected_chunk_ids: vec![chunk_id].into(),
                base_generation: None,
            },
            Arc::new(Cancelled),
        )
        .await
        .unwrap_err();

    assert_eq!(error, VectorGenerationStoreErrorV1::Cancelled);
    assert_eq!(
        store
            .active_generation_id(Arc::new(NeverCancelled))
            .await
            .unwrap(),
        Some(active)
    );
}

#[tokio::test]
async fn stale_publication_loses_without_displacing_the_cas_winner() {
    let store =
        GraphVectorGenerationStoreV1::open(memory_graph(), Arc::new(NeverCancelled)).unwrap();
    let initial = publish_one(
        &store,
        "code-generation.initial",
        "chunk.initial",
        'e',
        vec![1.0, 0.0],
        None,
    )
    .await;

    let winner = publish_one(
        &store,
        "code-generation.winner",
        "chunk.winner",
        'f',
        vec![0.0, 1.0],
        Some(&initial),
    )
    .await;

    let source = id::<CodeGenerationId>("code-generation.loser");
    let chunk_id = id::<CodeSearchChunkId>("chunk.loser");
    let prepared = added_prepared(&source, &chunk_id, &content_digest('9'), vec![0.5, 0.5]);
    let build = store
        .begin_generation(
            VectorGenerationPlanV1 {
                target_projection_key: prepared.embedding_key.projection_key().clone(),
                source_generation: source,
                source_manifest_digest: prepared.request.changes.manifest_digest.clone(),
                expected_chunk_ids: vec![chunk_id].into(),
                base_generation: None,
            },
            Arc::new(NeverCancelled),
        )
        .await
        .unwrap();
    store
        .commit_batch(&build, None, prepared, Arc::new(NeverCancelled))
        .await
        .unwrap();
    let error = store
        .publish_generation(&build, Some(&initial), Arc::new(NeverCancelled))
        .await
        .unwrap_err();

    assert_eq!(error, VectorGenerationStoreErrorV1::StaleActiveGeneration);
    assert_eq!(
        store
            .active_generation_id(Arc::new(NeverCancelled))
            .await
            .unwrap(),
        Some(winner)
    );
}

#[tokio::test]
async fn vector_search_is_generation_bound_and_rejects_foreign_privacy_authority() {
    let store =
        GraphVectorGenerationStoreV1::open(memory_graph(), Arc::new(NeverCancelled)).unwrap();
    let generation_id = publish_one(
        &store,
        "code-generation.search",
        "chunk.search",
        '8',
        vec![1.0, 0.0],
        None,
    )
    .await;
    let generation = store
        .active_generation(Arc::new(NeverCancelled))
        .await
        .unwrap()
        .unwrap();
    let result = store
        .search_active_vectors(SemanticVectorGraphSearchRequestV1 {
            generation_id: generation_id.clone(),
            embedding_key: generation.embedding_key().clone(),
            source_generation: generation.source_generation().clone(),
            source_manifest_digest: generation.source_manifest_digest().clone(),
            query: vec![1.0, 0.0],
            limit: 1,
            cancellation: Arc::new(NeverCancelled),
        })
        .await
        .unwrap();
    assert_eq!(result.generation_id, generation_id);
    assert_eq!(result.matches.len(), 1);
    assert_eq!(
        result.matches[0].chunk_id,
        id::<CodeSearchChunkId>("chunk.search")
    );

    let mut foreign_key = generation.embedding_key().embedding_key().clone();
    foreign_key.privacy_domain = id::<PrivacyDomainId>("privacy.project-foreign");
    let error = store
        .search_active_vectors(SemanticVectorGraphSearchRequestV1 {
            generation_id: result.generation_id,
            embedding_key: foreign_key.admit().unwrap(),
            source_generation: generation.source_generation().clone(),
            source_manifest_digest: generation.source_manifest_digest().clone(),
            query: vec![1.0, 0.0],
            limit: 1,
            cancellation: Arc::new(NeverCancelled),
        })
        .await
        .unwrap_err();
    assert_eq!(error, VectorGenerationStoreErrorV1::BatchIdentityMismatch);
}

#[tokio::test]
async fn resident_plan_and_publication_guard_pin_the_exact_active_graph_snapshot() {
    let store =
        GraphVectorGenerationStoreV1::open(memory_graph(), Arc::new(NeverCancelled)).unwrap();
    let generation_id = publish_one(
        &store,
        "code-generation.resident",
        "chunk.resident",
        '7',
        vec![0.25, 0.75],
        None,
    )
    .await;
    let metadata = store
        .active_generation(Arc::new(NeverCancelled))
        .await
        .unwrap()
        .unwrap();
    let plan = store
        .active_resident_plan(&generation_id, Arc::new(NeverCancelled))
        .await
        .unwrap()
        .unwrap();
    assert!(plan.retained_bytes > 0);
    assert!(plan.hydration_peak_bytes >= plan.retained_bytes);

    let guard = store
        .acquire_active_generation_publication_guard(
            &plan.watermark,
            &generation_id,
            Arc::new(NeverCancelled),
        )
        .unwrap();
    assert_eq!(guard.watermark(), &plan.watermark);
    assert_eq!(guard.generation_id(), &generation_id);
    assert_eq!(guard.projection_key(), metadata.projection_key());
    assert_eq!(guard.source_generation(), metadata.source_generation());
    assert_eq!(
        guard.source_manifest_digest(),
        metadata.source_manifest_digest()
    );
    assert_eq!(guard.embedding_key(), metadata.embedding_key());
    drop(guard);

    let resident = store
        .read_resident_generation_for(
            &plan,
            metadata.embedding_key(),
            metadata.source_generation(),
            metadata.source_manifest_digest(),
            Arc::new(NeverCancelled),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(resident.generation_id, generation_id);
    assert_eq!(resident.rows.len(), 1);
    assert_eq!(resident.rows[0].values.as_ref(), &[0.25, 0.75]);
    assert!(resident.retained_bytes <= plan.retained_bytes);
}

#[tokio::test]
async fn hybrid_search_filters_foreign_chunks_and_orders_equal_scores_by_chunk_identity() {
    let store =
        GraphVectorGenerationStoreV1::open(memory_graph(), Arc::new(NeverCancelled)).unwrap();
    let generation_id = publish_one(
        &store,
        "code-generation.hybrid",
        "chunk.hybrid",
        '6',
        vec![1.0, 0.0],
        None,
    )
    .await;
    let generation = store
        .active_generation(Arc::new(NeverCancelled))
        .await
        .unwrap()
        .unwrap();
    let result = store
        .search_active_hybrid(SemanticHybridGraphSearchRequestV1 {
            vector: SemanticVectorGraphSearchRequestV1 {
                generation_id: generation_id.clone(),
                embedding_key: generation.embedding_key().clone(),
                source_generation: generation.source_generation().clone(),
                source_manifest_digest: generation.source_manifest_digest().clone(),
                query: vec![1.0, 0.0],
                limit: 8,
                cancellation: Arc::new(NeverCancelled),
            },
            lexical: vec![
                SemanticHybridLexicalCandidateV1 {
                    chunk_id: id("chunk.foreign"),
                    score: 100.0,
                },
                SemanticHybridLexicalCandidateV1 {
                    chunk_id: id("chunk.hybrid"),
                    score: 1.0,
                },
            ],
            vector_weight: 0.5,
            lexical_weight: 0.5,
            limit: 8,
        })
        .await
        .unwrap();

    assert_eq!(result.generation_id, generation_id);
    assert_eq!(result.matches.len(), 1);
    assert_eq!(
        result.matches[0].chunk_id,
        id::<CodeSearchChunkId>("chunk.hybrid")
    );
    assert_eq!(result.matches[0].lexical_score, Some(1.0));
    assert_eq!(result.matches[0].combined_score, 1.0);
}

#[tokio::test]
async fn persistent_restart_preserves_native_vectors_and_exact_binding() {
    let root = tempfile::tempdir().unwrap();
    let registry = GraphDbRegistry::new(GraphDbRegistryConfig { max_open: 1 }).unwrap();
    let registration = graph_registration(root.path());
    let store = GraphVectorGenerationStoreV1::open(
        registry.resolve(registration.clone()).unwrap(),
        Arc::new(NeverCancelled),
    )
    .unwrap();
    let generation_id = publish_one(
        &store,
        "code-generation.restart",
        "chunk.restart",
        '5',
        vec![1.0, 0.0],
        None,
    )
    .await;
    let generation = store
        .active_generation(Arc::new(NeverCancelled))
        .await
        .unwrap()
        .unwrap();
    drop(store);
    assert!(registry.close(&registration).unwrap());

    let reopened = GraphVectorGenerationStoreV1::open(
        registry.resolve(registration.clone()).unwrap(),
        Arc::new(NeverCancelled),
    )
    .unwrap();
    let result = reopened
        .search_active_vectors(SemanticVectorGraphSearchRequestV1 {
            generation_id,
            embedding_key: generation.embedding_key().clone(),
            source_generation: generation.source_generation().clone(),
            source_manifest_digest: generation.source_manifest_digest().clone(),
            query: vec![1.0, 0.0],
            limit: 1,
            cancellation: Arc::new(NeverCancelled),
        })
        .await
        .unwrap();

    assert_eq!(result.matches.len(), 1);
    assert_eq!(
        result.matches[0].chunk_id,
        id::<CodeSearchChunkId>("chunk.restart")
    );
    drop(reopened);
    assert!(registry.close(&registration).unwrap());
}

#[tokio::test]
async fn reclaim_removes_one_deterministic_unretained_generation_and_reports_exact_effects() {
    let store =
        GraphVectorGenerationStoreV1::open(memory_graph(), Arc::new(NeverCancelled)).unwrap();
    let first = publish_one(
        &store,
        "code-generation.reclaim-first",
        "chunk.reclaim-first",
        '2',
        vec![1.0, 0.0],
        None,
    )
    .await;
    let active = publish_one(
        &store,
        "code-generation.reclaim-active",
        "chunk.reclaim-active",
        '3',
        vec![0.0, 1.0],
        Some(&first),
    )
    .await;

    let receipt = store
        .reclaim_unretained_generations(
            Some(&active),
            &SemanticRetainedVectorGenerationsV1::default(),
            Arc::new(NeverCancelled),
        )
        .await
        .unwrap();

    assert_eq!(receipt.reclaimed_generation_ids, vec![first.clone()]);
    assert_eq!(receipt.rows, 1);
    assert_eq!(receipt.vector_bytes, 2 * size_of::<f32>() as u64);
    assert_eq!(receipt.remaining, 0);
    assert!(
        store
            .generation(&first, Arc::new(NeverCancelled))
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .active_generation_id(Arc::new(NeverCancelled))
            .await
            .unwrap(),
        Some(active)
    );
}

#[tokio::test]
async fn missing_active_search_row_fails_closed_instead_of_returning_empty_success() {
    let graph = memory_graph();
    let store =
        GraphVectorGenerationStoreV1::open(Arc::clone(&graph), Arc::new(NeverCancelled)).unwrap();
    let generation_id = publish_one(
        &store,
        "code-generation.corrupt",
        "chunk.corrupt",
        '4',
        vec![1.0, 0.0],
        None,
    )
    .await;
    let generation = store
        .active_generation(Arc::new(NeverCancelled))
        .await
        .unwrap()
        .unwrap();
    let active_watermark = store
        .active_resident_plan(&generation_id, Arc::new(NeverCancelled))
        .await
        .unwrap()
        .unwrap()
        .watermark;
    let page = graph
        .projection_entities_by_label(
            &GraphNamespace::new("tracedecay.semantic-vector.graph").unwrap(),
            &GraphProjectionId::new("tracedecay.semantic-vector.graph").unwrap(),
            &GraphLabel::new(format!(
                "semantic-vector-generation:{}",
                generation_id.as_digest().as_str()
            ))
            .unwrap(),
            1,
            Arc::new(NeverCancelled),
        )
        .unwrap();
    let mut corrupt_row = page.entities.into_iter().next().unwrap();
    corrupt_row
        .properties
        .remove(&GraphPropertyName::new("vector").unwrap());
    graph
        .apply_unverified(
            GraphWriteBatch::new(
                GraphNamespace::new("tracedecay.semantic-vector.graph").unwrap(),
                GraphProjectionId::new("tracedecay.semantic-vector.graph").unwrap(),
                SourceGeneration::new("code-generation.corrupt").unwrap(),
                active_watermark,
                vec![GraphMutation::UpsertEntity(corrupt_row)],
                Arc::new(NeverCancelled),
            )
            .unwrap(),
        )
        .unwrap();

    let error = store
        .search_active_vectors(SemanticVectorGraphSearchRequestV1 {
            generation_id,
            embedding_key: generation.embedding_key().clone(),
            source_generation: generation.source_generation().clone(),
            source_manifest_digest: generation.source_manifest_digest().clone(),
            query: vec![1.0, 0.0],
            limit: 1,
            cancellation: Arc::new(NeverCancelled),
        })
        .await
        .unwrap_err();

    assert!(matches!(error, VectorGenerationStoreErrorV1::Corrupt(_)));
}
