use tracedecay_domain::{
    BrainId, ProjectId, RepositoryId, UserProfileId, VectorGenerationIdV1, WorktreeId,
    canonical_sha256,
};
use tracedecay_graph_db::{
    GraphDbError, GraphGenerationDependency, GraphGenerationId, GraphIdempotencyKey,
    GraphNamespace, GraphProjectionId, GraphProjectionIdentity,
};
use tracedecay_store::{
    CodeShardScopeV1, GraphGenerationIdV1, GraphNamespaceV1, GraphProjectionIdV1,
    GraphProjectionIdentityV1, GraphPublicationIdempotencyKeyV1, GraphPublicationKeyV1,
    SemanticVectorBuildId, SemanticVectorReconstructionRecipe, SemanticVectorSourceGenerationId,
    SemanticVectorStagePlan, SemanticVectorWriterFence, StoreRuntimeBindingV1, StoreShardIdV1,
    semantic_vector_chunk_manifest_digest,
};

use super::{post_commit_publication_settlement_error, semantic_stage_source_identity};
use crate::store::vector_generations::VectorGenerationStoreErrorV1;

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

fn digest<T: TryFrom<String>>(byte: char) -> T
where
    T::Error: std::fmt::Debug,
{
    T::try_from(format!("sha256:{}", byte.to_string().repeat(64))).unwrap()
}
