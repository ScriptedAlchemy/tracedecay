use std::collections::{BTreeMap, BTreeSet};

use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, CodeSearchChunkId, ContentDigest, EmbeddingMetricV1,
    ProjectionBatchReceiptV1, ProjectionKeyV1, ProjectionOperationV1, ProjectionOutcomeV1,
    canonical_sha256, projection_batch_publication_digest, semantic_vector_output_digest,
};
use tracedecay_store::{
    SemanticVectorStageBatchReceipt, SemanticVectorStageChunkOperation, SemanticVectorStagePlan,
};

use crate::{
    GraphDbError, GraphEntity, GraphEntityId, GraphMutation, GraphProperty, GraphRelation,
    GraphWriteBatch, VectorMetric, semantic_vector_native,
    semantic_vector_native::{
        BASE_GENERATION, CHECKPOINT, CHUNK_DIGEST, CHUNK_ID, CONTAINS_KIND, CONTROL_ID,
        CONTROL_LABEL, EMBEDDING_KEY, GENERATION_CATALOG_KIND, GENERATION_ID, GENERATION_LABEL,
        GENERATION_RECEIPT_LABEL, GENERATION_TOMBSTONE_LABEL, GENERATION_VECTOR_LABEL,
        MANIFEST_DIGEST, ORDINAL, OUTPUT_DIGEST, PRIOR_DIGEST, RECEIPT, RECEIPT_COUNT, REVISION,
        ROW_COUNT, SOURCE_GENERATION, SOURCE_MANIFEST, TARGET_PROJECTION, TOMBSTONE_COUNT,
        VECTOR_BYTES,
    },
};

pub(super) fn validate_semantic_native_batch(
    plan: &SemanticVectorStagePlan,
    receipt: &SemanticVectorStageBatchReceipt,
    batch: &GraphWriteBatch,
) -> Result<(), GraphDbError> {
    let generation = plan.semantic_generation_id.as_digest().as_str();
    let owner_id = semantic_vector_native::generation_entity_id(generation)?.to_string();
    let generation_row_label = semantic_vector_native::generation_label(generation)?.to_string();
    let vector_property = semantic_vector_native::vector_property(generation)?.to_string();
    let receipt_id = semantic_vector_native::scoped_entity_id(
        "generation-receipt",
        generation,
        &receipt.key.ordinal.to_string(),
    )?
    .to_string();
    let mut entities = BTreeMap::new();
    let mut relations = BTreeMap::new();
    for mutation in &batch.mutations {
        match mutation {
            GraphMutation::UpsertEntity(entity) => {
                entity.validate()?;
                if entities.insert(entity.identity.as_str(), entity).is_some() {
                    return Err(GraphDbError::Conflict);
                }
            }
            GraphMutation::UpsertRelation(relation) => {
                relation.validate()?;
                if relations
                    .insert(relation.identity.as_str(), relation)
                    .is_some()
                {
                    return Err(GraphDbError::Conflict);
                }
            }
            GraphMutation::DeleteEntity(_) | GraphMutation::DeleteRelation(_) => {
                return Err(GraphDbError::Conflict);
            }
        }
    }

    let control = entities.remove(CONTROL_ID).ok_or(GraphDbError::Conflict)?;
    require_labels(control, &[CONTROL_LABEL])?;
    require_properties(control, &[REVISION])?;
    require_nonnegative_i64(control, REVISION)?;

    let owner = entities
        .remove(owner_id.as_str())
        .ok_or(GraphDbError::Conflict)?;
    require_labels(owner, &[GENERATION_LABEL])?;
    require_properties(
        owner,
        &[
            GENERATION_ID,
            TARGET_PROJECTION,
            SOURCE_GENERATION,
            SOURCE_MANIFEST,
            BASE_GENERATION,
            EMBEDDING_KEY,
            CHECKPOINT,
            MANIFEST_DIGEST,
            ROW_COUNT,
            VECTOR_BYTES,
            TOMBSTONE_COUNT,
            RECEIPT_COUNT,
        ],
    )?;
    require_string(owner, GENERATION_ID, generation)?;
    require_string(owner, MANIFEST_DIGEST, generation)?;
    require_string(owner, SOURCE_GENERATION, plan.source_generation.as_str())?;
    require_string(
        owner,
        SOURCE_MANIFEST,
        plan.recipe.source_manifest_digest.as_str(),
    )?;
    require_string(
        owner,
        BASE_GENERATION,
        plan.base_generation
            .as_ref()
            .map(|id| id.as_digest().as_str())
            .unwrap_or(""),
    )?;
    for count in [ROW_COUNT, VECTOR_BYTES, TOMBSTONE_COUNT, RECEIPT_COUNT] {
        require_nonnegative_i64(owner, count)?;
    }
    require_bytes(owner, CHECKPOINT)?;
    let target_projection: ProjectionKeyV1 = decode_bytes(owner, TARGET_PROJECTION)?;
    let embedding: AdmittedEmbeddingProjectionKeyV1 = decode_bytes(owner, EMBEDDING_KEY)?;
    validate_embedding_admission(plan, &target_projection, &embedding)?;

    let receipt_row = entities
        .remove(receipt_id.as_str())
        .ok_or(GraphDbError::Conflict)?;
    require_labels(receipt_row, &[GENERATION_RECEIPT_LABEL])?;
    require_properties(receipt_row, &[GENERATION_ID, RECEIPT, ORDINAL])?;
    require_string(receipt_row, GENERATION_ID, generation)?;
    require_i64(
        receipt_row,
        ORDINAL,
        i64::try_from(receipt.key.ordinal).map_err(|_| GraphDbError::Conflict)?,
    )?;
    let projection_receipt = decode_generation_receipt(receipt_row)?;
    validate_projection_receipt(plan, receipt, &target_projection, &projection_receipt)?;

    let expected_metric = graph_metric(embedding.embedding_key().metric);
    let expected_dimension = usize::from(plan.recipe.embedding_dimension);
    let mut expected_effect_ids = Vec::with_capacity(receipt.chunks.len());
    for chunk in &receipt.chunks {
        let chunk_id = chunk.chunk_id.as_str();
        let (kind, discriminator) = match chunk.operation {
            SemanticVectorStageChunkOperation::Embed => ("generation-vector", "vector"),
            SemanticVectorStageChunkOperation::Tombstone => ("generation-tombstone", "tombstone"),
        };
        let effect_id =
            semantic_vector_native::scoped_entity_id(kind, generation, chunk_id)?.to_string();
        let effect = entities
            .remove(effect_id.as_str())
            .ok_or(GraphDbError::Conflict)?;
        match chunk.operation {
            SemanticVectorStageChunkOperation::Embed => validate_vector_effect(
                effect,
                generation,
                &generation_row_label,
                &vector_property,
                chunk_id,
                chunk.chunk_digest.as_str(),
                chunk
                    .output_digest
                    .as_ref()
                    .ok_or(GraphDbError::Conflict)?
                    .as_str(),
                expected_dimension,
                expected_metric,
                &target_projection,
            )?,
            SemanticVectorStageChunkOperation::Tombstone => validate_tombstone_effect(
                effect,
                generation,
                chunk_id,
                chunk.chunk_digest.as_str(),
            )?,
        }
        expected_effect_ids.push((effect_id, discriminator));
    }
    if !entities.is_empty() {
        return Err(GraphDbError::Conflict);
    }

    require_relation(
        &mut relations,
        CONTROL_ID,
        &owner_id,
        GENERATION_CATALOG_KIND,
        "generation-catalog",
    )?;
    require_relation(
        &mut relations,
        &owner_id,
        &receipt_id,
        CONTAINS_KIND,
        "batch",
    )?;
    for (effect_id, discriminator) in expected_effect_ids {
        require_relation(
            &mut relations,
            &owner_id,
            &effect_id,
            CONTAINS_KIND,
            discriminator,
        )?;
    }
    if !relations.is_empty() {
        return Err(GraphDbError::Conflict);
    }
    Ok(())
}

fn validate_embedding_admission(
    plan: &SemanticVectorStagePlan,
    target: &ProjectionKeyV1,
    embedding: &AdmittedEmbeddingProjectionKeyV1,
) -> Result<(), GraphDbError> {
    let embedding_digest = canonical_sha256(embedding).map_err(|_| GraphDbError::Conflict)?;
    let privacy_digest =
        canonical_sha256(embedding.privacy_domain()).map_err(|_| GraphDbError::Conflict)?;
    let key = embedding.embedding_key();
    if embedding.projection_key() != target
        || target.profile_digest.as_str() != plan.recipe.projection_manifest_digest.as_str()
        || embedding_digest.as_str() != plan.recipe.embedding_projection_digest.as_str()
        || key.model_artifact_digest.as_str() != plan.recipe.model_artifact_digest.as_str()
        || key.dimensions != u32::from(plan.recipe.embedding_dimension)
        || privacy_digest.as_str() != plan.recipe.privacy_domain_digest.as_str()
        || embedding.privacy_key_epoch() != plan.recipe.privacy_key_epoch
    {
        return Err(GraphDbError::Conflict);
    }
    Ok(())
}

fn validate_projection_receipt(
    plan: &SemanticVectorStagePlan,
    stage: &SemanticVectorStageBatchReceipt,
    target: &ProjectionKeyV1,
    projection: &ProjectionBatchReceiptV1,
) -> Result<(), GraphDbError> {
    // The stage recipe's source_manifest_digest is the corpus watermark
    // (the unsplit change-set). Each page receipt carries that page's
    // change-set digest so ChangedCodeChunkSetV1::validate stays
    // self-consistent. They are equal only for a one-batch corpus.
    if &projection.target_projection_key != target
        || projection.source_generation.to_string() != plan.source_generation.as_str()
        || projection_batch_publication_digest(projection).map_err(|_| GraphDbError::Conflict)?
            != projection.publication_digest
        || projection.receipts.len() != stage.chunks.len()
        || projection
            .receipts
            .windows(2)
            .any(|pair| pair[0].chunk_id >= pair[1].chunk_id)
        || projection.reused_count
            != u64::try_from(
                projection
                    .receipts
                    .iter()
                    .filter(|chunk| chunk.operation == ProjectionOperationV1::Reused)
                    .count(),
            )
            .map_err(|_| GraphDbError::Conflict)?
    {
        return Err(GraphDbError::Conflict);
    }
    for (native, durable) in projection.receipts.iter().zip(&stage.chunks) {
        let operation_matches = matches!(
            (native.operation, durable.operation),
            (
                ProjectionOperationV1::Added
                    | ProjectionOperationV1::Updated
                    | ProjectionOperationV1::Reused,
                SemanticVectorStageChunkOperation::Embed
            ) | (
                ProjectionOperationV1::Deleted,
                SemanticVectorStageChunkOperation::Tombstone
            )
        );
        let digest_matches = match native.operation {
            ProjectionOperationV1::Added => {
                native
                    .current_chunk_digest
                    .as_ref()
                    .map(ContentDigest::as_str)
                    == Some(durable.chunk_digest.as_str())
                    && native.prior_chunk_digest.is_none()
                    && native.output_digest.as_ref().map(ContentDigest::as_str)
                        == durable.output_digest.as_ref().map(|digest| digest.as_str())
                    && matches!(&native.outcome, ProjectionOutcomeV1::Applied)
            }
            ProjectionOperationV1::Updated => {
                native
                    .current_chunk_digest
                    .as_ref()
                    .map(ContentDigest::as_str)
                    == Some(durable.chunk_digest.as_str())
                    && native.prior_chunk_digest.is_some()
                    && native.output_digest.as_ref().map(ContentDigest::as_str)
                        == durable.output_digest.as_ref().map(|digest| digest.as_str())
                    && matches!(&native.outcome, ProjectionOutcomeV1::Applied)
            }
            ProjectionOperationV1::Reused => {
                native
                    .current_chunk_digest
                    .as_ref()
                    .map(ContentDigest::as_str)
                    == Some(durable.chunk_digest.as_str())
                    && native.prior_chunk_digest == native.current_chunk_digest
                    && native.output_digest.is_none()
                    && matches!(&native.outcome, ProjectionOutcomeV1::Reused)
            }
            ProjectionOperationV1::Deleted => {
                native
                    .prior_chunk_digest
                    .as_ref()
                    .map(ContentDigest::as_str)
                    == Some(durable.chunk_digest.as_str())
                    && native.current_chunk_digest.is_none()
                    && native.output_digest.is_none()
                    && matches!(&native.outcome, ProjectionOutcomeV1::Applied)
            }
        };
        if !operation_matches
            || !digest_matches
            || native.chunk_id.to_string() != durable.chunk_id.as_str()
            || native.projection_key != projection.target_projection_key
            || native.request_digest != projection.request_digest
            || native.source_generation != projection.source_generation
            || native.source_manifest_digest != projection.source_manifest_digest
        {
            return Err(GraphDbError::Conflict);
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn validate_vector_effect(
    effect: &GraphEntity,
    generation: &str,
    generation_label: &str,
    vector_property: &str,
    chunk_id: &str,
    chunk_digest: &str,
    output_digest: &str,
    dimension: usize,
    metric: VectorMetric,
    projection: &ProjectionKeyV1,
) -> Result<(), GraphDbError> {
    require_labels(effect, &[GENERATION_VECTOR_LABEL, generation_label])?;
    require_properties(
        effect,
        &[
            GENERATION_ID,
            CHUNK_ID,
            CHUNK_DIGEST,
            OUTPUT_DIGEST,
            vector_property,
        ],
    )?;
    require_string(effect, GENERATION_ID, generation)?;
    require_string(effect, CHUNK_ID, chunk_id)?;
    require_string(effect, CHUNK_DIGEST, chunk_digest)?;
    require_string(effect, OUTPUT_DIGEST, output_digest)?;
    match property(effect, vector_property)? {
        GraphProperty::Vector(vector)
            if vector.dimension == dimension
                && vector.values.len() == dimension
                && vector.metric == metric
                && semantic_vector_output_digest(
                    projection,
                    &CodeSearchChunkId::try_from(chunk_id.to_owned())
                        .map_err(|_| GraphDbError::Conflict)?,
                    &ContentDigest::try_from(chunk_digest.to_owned())
                        .map_err(|_| GraphDbError::Conflict)?,
                    &vector.values,
                )
                .map_err(|_| GraphDbError::Conflict)?
                .as_str()
                    == output_digest =>
        {
            Ok(())
        }
        _ => Err(GraphDbError::Conflict),
    }
}

fn validate_tombstone_effect(
    effect: &GraphEntity,
    generation: &str,
    chunk_id: &str,
    prior_digest: &str,
) -> Result<(), GraphDbError> {
    require_labels(effect, &[GENERATION_TOMBSTONE_LABEL])?;
    require_properties(effect, &[GENERATION_ID, CHUNK_ID, PRIOR_DIGEST])?;
    require_string(effect, GENERATION_ID, generation)?;
    require_string(effect, CHUNK_ID, chunk_id)?;
    require_string(effect, PRIOR_DIGEST, prior_digest)
}

fn require_relation<'a>(
    relations: &mut BTreeMap<&'a str, &'a GraphRelation>,
    from: &str,
    to: &str,
    kind: &str,
    discriminator: &str,
) -> Result<(), GraphDbError> {
    let from_id = GraphEntityId::new(from)?;
    let to_id = GraphEntityId::new(to)?;
    let id = semantic_vector_native::relation_id(&from_id, &to_id, kind, discriminator)?;
    let relation = relations
        .remove(id.as_str())
        .ok_or(GraphDbError::Conflict)?;
    if relation.from.as_str() != from
        || relation.to.as_str() != to
        || relation.kind.as_str() != kind
        || !relation.properties.is_empty()
    {
        return Err(GraphDbError::Conflict);
    }
    Ok(())
}

fn require_labels(entity: &GraphEntity, expected: &[&str]) -> Result<(), GraphDbError> {
    let actual = entity
        .labels
        .iter()
        .map(|label| label.as_str())
        .collect::<BTreeSet<_>>();
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    if actual == expected {
        Ok(())
    } else {
        Err(GraphDbError::Conflict)
    }
}

fn require_properties(entity: &GraphEntity, expected: &[&str]) -> Result<(), GraphDbError> {
    let actual = entity
        .properties
        .keys()
        .map(|name| name.as_str())
        .collect::<BTreeSet<_>>();
    let expected = expected.iter().copied().collect::<BTreeSet<_>>();
    if actual == expected {
        Ok(())
    } else {
        Err(GraphDbError::Conflict)
    }
}

fn property<'a>(entity: &'a GraphEntity, name: &str) -> Result<&'a GraphProperty, GraphDbError> {
    entity
        .properties
        .iter()
        .find_map(|(key, value)| (key.as_str() == name).then_some(value))
        .ok_or(GraphDbError::Conflict)
}

fn require_string(entity: &GraphEntity, name: &str, expected: &str) -> Result<(), GraphDbError> {
    match property(entity, name)? {
        GraphProperty::String(actual) if actual == expected => Ok(()),
        _ => Err(GraphDbError::Conflict),
    }
}

fn require_i64(entity: &GraphEntity, name: &str, expected: i64) -> Result<(), GraphDbError> {
    match property(entity, name)? {
        GraphProperty::I64(actual) if *actual == expected => Ok(()),
        _ => Err(GraphDbError::Conflict),
    }
}

fn require_nonnegative_i64(entity: &GraphEntity, name: &str) -> Result<(), GraphDbError> {
    match property(entity, name)? {
        GraphProperty::I64(actual) if *actual >= 0 => Ok(()),
        _ => Err(GraphDbError::Conflict),
    }
}

fn require_bytes(entity: &GraphEntity, name: &str) -> Result<(), GraphDbError> {
    match property(entity, name)? {
        GraphProperty::Bytes(_) => Ok(()),
        _ => Err(GraphDbError::Conflict),
    }
}

fn decode_bytes<T: serde::de::DeserializeOwned>(
    entity: &GraphEntity,
    name: &str,
) -> Result<T, GraphDbError> {
    match property(entity, name)? {
        GraphProperty::Bytes(bytes) => {
            serde_json::from_slice(bytes).map_err(|_| GraphDbError::Conflict)
        }
        _ => Err(GraphDbError::Conflict),
    }
}

fn decode_generation_receipt(
    entity: &GraphEntity,
) -> Result<ProjectionBatchReceiptV1, GraphDbError> {
    match property(entity, RECEIPT)? {
        GraphProperty::Bytes(bytes) => semantic_vector_native::decode_generation_receipt(bytes),
        _ => Err(GraphDbError::Conflict),
    }
}

const fn graph_metric(metric: EmbeddingMetricV1) -> VectorMetric {
    match metric {
        EmbeddingMetricV1::Cosine => VectorMetric::Cosine,
        EmbeddingMetricV1::DotProduct => VectorMetric::DotProduct,
        EmbeddingMetricV1::EuclideanL2 => VectorMetric::Euclidean,
    }
}
