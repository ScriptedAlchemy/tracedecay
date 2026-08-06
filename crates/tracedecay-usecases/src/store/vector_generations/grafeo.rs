//! Grafeo-owned semantic vector publication and retirement.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use tracedecay_domain::{
    AdmittedEmbeddingProjectionKeyV1, CodeSearchChunkId, EmbeddingMetricV1, ManifestDigest,
};
use tracedecay_graph_db::{
    GraphDb, GraphEntity, GraphEntityId, GraphIdempotencyKey, GraphLabel, GraphMutation,
    GraphNamespace, GraphProjectionId, GraphProjectionTelemetryRequest, GraphProperty,
    GraphPropertyName, GraphPublication, GraphVector, GraphWatermark, GraphWriteBatch,
    NeverCancelled, ProjectionReplacement, SourceGeneration, VectorMetric,
};
use tracedecay_semantic::projector::{PreparedVectorGenerationV1, ProjectedChunkVectorV1};

use super::{VectorGenerationBuildIdV1, VectorGenerationStoreErrorV1, graph_store_error};

pub(super) const SEMANTIC_VECTOR_NAMESPACE: &str = "semantic-code-vectors";
const SEMANTIC_VECTOR_LABEL: &str = "semantic-vector";
const SEMANTIC_VECTOR_PROPERTY: &str = "embedding";
const SEMANTIC_VECTOR_OUTPUT_DIGEST_PROPERTY: &str = "output-digest";
const SEMANTIC_VECTOR_CHUNK_PROPERTY: &str = "chunk-id";
const GRAPH_VECTOR_ENTITIES_PER_BATCH: usize = 256;

pub(super) fn graph_vector_metric(metric: EmbeddingMetricV1) -> VectorMetric {
    match metric {
        EmbeddingMetricV1::Cosine => VectorMetric::Cosine,
        EmbeddingMetricV1::DotProduct => VectorMetric::DotProduct,
        EmbeddingMetricV1::EuclideanL2 => VectorMetric::Euclidean,
    }
}

pub(crate) fn graph_projection_id(
    kind: &str,
    digest: &ManifestDigest,
) -> Result<GraphProjectionId, VectorGenerationStoreErrorV1> {
    GraphProjectionId::new(format!("{kind}:{}", digest.as_str())).map_err(graph_store_error)
}

pub(super) fn build_projection_id(
    build: &VectorGenerationBuildIdV1,
) -> Result<GraphProjectionId, VectorGenerationStoreErrorV1> {
    graph_projection_id("semantic-vector-generation", &build.0)
}

pub(crate) fn graph_vector_entity_id(
    projection: &GraphProjectionId,
    chunk_id: &CodeSearchChunkId,
) -> Result<GraphEntityId, VectorGenerationStoreErrorV1> {
    GraphEntityId::new(format!("{}:{}", projection.as_str(), chunk_id.as_str()))
        .map_err(graph_store_error)
}

fn graph_vector_entity(
    projection: &GraphProjectionId,
    embedding_key: &AdmittedEmbeddingProjectionKeyV1,
    vector: &ProjectedChunkVectorV1,
) -> Result<GraphEntity, VectorGenerationStoreErrorV1> {
    let dimensions = usize::try_from(embedding_key.embedding_key().dimensions)
        .map_err(|error| VectorGenerationStoreErrorV1::Storage(error.to_string()))?;
    GraphEntity::new(
        graph_vector_entity_id(projection, &vector.chunk_id)?,
        BTreeSet::from([GraphLabel::new(SEMANTIC_VECTOR_LABEL).map_err(graph_store_error)?]),
        BTreeMap::from([
            (
                GraphPropertyName::new(SEMANTIC_VECTOR_PROPERTY).map_err(graph_store_error)?,
                GraphProperty::Vector(
                    GraphVector::new(
                        vector.values.clone(),
                        dimensions,
                        graph_vector_metric(embedding_key.embedding_key().metric),
                    )
                    .map_err(graph_store_error)?,
                ),
            ),
            (
                GraphPropertyName::new(SEMANTIC_VECTOR_OUTPUT_DIGEST_PROPERTY)
                    .map_err(graph_store_error)?,
                GraphProperty::String(vector.output_digest.as_str().to_owned()),
            ),
            (
                GraphPropertyName::new(SEMANTIC_VECTOR_CHUNK_PROPERTY)
                    .map_err(graph_store_error)?,
                GraphProperty::String(vector.chunk_id.as_str().to_owned()),
            ),
        ]),
    )
    .map_err(graph_store_error)
}

pub(super) fn batch_watermark(
    request_digest: &ManifestDigest,
) -> Result<GraphWatermark, VectorGenerationStoreErrorV1> {
    GraphWatermark::new(format!("semantic-vector-batch:{}", request_digest.as_str()))
        .map_err(graph_store_error)
}

pub(super) fn graph_chunk_count(prepared: &PreparedVectorGenerationV1) -> u64 {
    u64::try_from(
        prepared
            .vectors
            .len()
            .max(1)
            .div_ceil(GRAPH_VECTOR_ENTITIES_PER_BATCH),
    )
    .unwrap_or(u64::MAX)
}

/// Idempotently publish one prepared delta in bounded graph transactions.
///
/// The projection is the deterministic build identity, so later batches append
/// to the same immutable generation delta. Replays use the request digest and
/// chunk ordinal as their Graph idempotency keys.
pub(super) fn prepared_delta_publications(
    namespace: &GraphNamespace,
    build: &VectorGenerationBuildIdV1,
    prior_watermark: Option<GraphWatermark>,
    prepared: &PreparedVectorGenerationV1,
) -> Result<(GraphProjectionId, Vec<GraphPublication>), VectorGenerationStoreErrorV1> {
    let projection = build_projection_id(build)?;
    let source_generation = SourceGeneration::new(prepared.request.changes.to_generation.as_str())
        .map_err(graph_store_error)?;
    let next_watermark = batch_watermark(&prepared.request.request_digest)?;
    let vector_chunks = if prepared.vectors.is_empty() {
        vec![prepared.vectors.as_slice()]
    } else {
        prepared
            .vectors
            .chunks(GRAPH_VECTOR_ENTITIES_PER_BATCH)
            .collect::<Vec<_>>()
    };
    let chunk_count = vector_chunks.len();
    let mut publications = Vec::with_capacity(chunk_count);
    for (ordinal, vectors) in vector_chunks.into_iter().enumerate() {
        let mutations = vectors
            .iter()
            .map(|vector| {
                graph_vector_entity(&projection, &prepared.embedding_key, vector)
                    .map(GraphMutation::UpsertEntity)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let expected_watermark = if ordinal == 0 {
            prior_watermark.clone()
        } else {
            Some(next_watermark.clone())
        };
        let batch = GraphWriteBatch::new(
            namespace.clone(),
            projection.clone(),
            source_generation.clone(),
            next_watermark.clone(),
            mutations,
            Arc::new(NeverCancelled),
        )
        .map_err(graph_store_error)?;
        publications.push(GraphPublication {
            namespace: namespace.clone(),
            idempotency_key: GraphIdempotencyKey::new(format!(
                "semantic-vector:{}:{}:{}",
                build.0.as_str(),
                prepared.request.request_digest.as_str(),
                ordinal
            ))
            .map_err(graph_store_error)?,
            source_generation: source_generation.clone(),
            expected_watermark,
            next_watermark: next_watermark.clone(),
            batch,
            cancellation: Arc::new(NeverCancelled),
        });
    }
    Ok((projection, publications))
}

pub(super) fn verify_projection(
    graph: &GraphDb,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    source: &SourceGeneration,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let telemetry = graph
        .projection_telemetry(GraphProjectionTelemetryRequest {
            namespace: namespace.clone(),
            projection: projection.clone(),
            cancellation: Arc::new(NeverCancelled),
        })
        .map_err(graph_store_error)?
        .ok_or_else(|| {
            VectorGenerationStoreErrorV1::Corrupt(format!(
                "Grafeo projection {projection} is missing"
            ))
        })?;
    if &telemetry.source_generation != source || telemetry.relation_count != 0 {
        return Err(VectorGenerationStoreErrorV1::Corrupt(format!(
            "Grafeo projection {projection} has incompatible telemetry"
        )));
    }
    Ok(())
}

pub(super) fn retire_projection(
    graph: &GraphDb,
    namespace: &GraphNamespace,
    projection: GraphProjectionId,
    source_generation: SourceGeneration,
    revision: i64,
) -> Result<(), VectorGenerationStoreErrorV1> {
    graph
        .replace_projection(ProjectionReplacement {
            namespace: namespace.clone(),
            projection,
            source_generation,
            next_watermark: GraphWatermark::new(format!(
                "semantic-vector-retired:{}",
                revision.saturating_add(1)
            ))
            .map_err(graph_store_error)?,
            entities: Vec::new(),
            relations: Vec::new(),
            cancellation: Arc::new(NeverCancelled),
        })
        .map_err(|error| {
            VectorGenerationStoreErrorV1::DurabilityUncertain(format!(
                "relational vector state committed but Grafeo retirement failed: {error}"
            ))
        })?;
    Ok(())
}
