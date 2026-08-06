use std::mem::size_of;
use std::sync::Arc;

use tracedecay_domain::{EmbeddingMetricV1, VectorGenerationIdV1, canonical_sha256};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphEntity, GraphIdempotencyKey, GraphLabel, GraphMutation,
    GraphNamespace, GraphProjectionId, GraphProperty, GraphPropertyName, GraphPublication,
    GraphPublicationInputDigest, GraphWatermark, GraphWriteBatch, SourceGeneration, VectorMetric,
};

use super::super::{VectorGenerationStateMachineV1, VectorGenerationStoreErrorV1};
use super::native_records::{NativeGraphStateV1, encode_state};
use super::{
    GraphVectorGenerationStoreV1, ResidentVectorRowV1, SEMANTIC_VECTOR_GRAPH_PROJECTION,
    VECTOR_PROPERTY,
};

const SEMANTIC_VECTOR_GRAPH_STATE_DIGEST_DOMAIN: &str =
    "tracedecay.semantic-vector.graph-native-state.v1";

impl GraphVectorGenerationStoreV1 {
    pub(super) fn initialize_state(
        &self,
        state: &mut VectorGenerationStateMachineV1,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<GraphWatermark, VectorGenerationStoreErrorV1> {
        check_cancelled(cancellation.as_ref())?;
        let encoded = encode_state(state, 0)?;
        let (next_watermark, state_digest) = native_state_watermark_from_encoded(&encoded)?;
        let mut mutations = encoded
            .entities
            .into_iter()
            .map(GraphMutation::UpsertEntity)
            .collect::<Vec<_>>();
        mutations.extend(
            encoded
                .relations
                .into_iter()
                .map(GraphMutation::UpsertRelation),
        );
        let batch = GraphWriteBatch::new(
            graph_namespace()?,
            graph_projection()?,
            SourceGeneration::new("semantic-vector-unpublished").map_err(map_graph_error)?,
            next_watermark.clone(),
            mutations,
            Arc::clone(&cancellation),
        )
        .map_err(map_graph_error)?;
        let publication = GraphPublication {
            namespace: graph_namespace()?,
            idempotency_key: GraphIdempotencyKey::new(format!(
                "semantic-vector:{}",
                state_digest.as_str()
            ))
            .map_err(map_graph_error)?,
            input_digest: GraphPublicationInputDigest::new(state_digest.as_str())
                .map_err(map_graph_error)?,
            source_generation: batch.source_generation.clone(),
            expected_watermark: None,
            next_watermark,
            batch,
            cancellation,
        };
        self.graph
            .publish_unverified(publication)
            .map(|commit| commit.watermark)
            .map_err(|error| match error {
                GraphDbError::Conflict => VectorGenerationStoreErrorV1::ConcurrentMutation,
                other => map_graph_error(other),
            })
    }

    pub(super) fn publish_record_mutations(
        &self,
        revision: u64,
        expected_watermark: GraphWatermark,
        source_generation: String,
        input_digest: tracedecay_domain::ManifestDigest,
        mutations: Vec<GraphMutation>,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<GraphWatermark, VectorGenerationStoreErrorV1> {
        check_cancelled(cancellation.as_ref())?;
        let next_watermark = GraphWatermark::new(format!(
            "semantic-vector:{revision}:{}",
            input_digest.as_str()
        ))
        .map_err(map_graph_error)?;
        let batch = GraphWriteBatch::new(
            graph_namespace()?,
            graph_projection()?,
            SourceGeneration::new(source_generation).map_err(map_graph_error)?,
            next_watermark.clone(),
            mutations,
            Arc::clone(&cancellation),
        )
        .map_err(map_graph_error)?;
        self.graph
            .publish_unverified(GraphPublication {
                namespace: graph_namespace()?,
                idempotency_key: GraphIdempotencyKey::new(format!(
                    "semantic-vector:{}",
                    input_digest.as_str()
                ))
                .map_err(map_graph_error)?,
                input_digest: GraphPublicationInputDigest::new(input_digest.as_str())
                    .map_err(map_graph_error)?,
                source_generation: batch.source_generation.clone(),
                expected_watermark: Some(expected_watermark),
                next_watermark,
                batch,
                cancellation,
            })
            .map(|commit| commit.watermark)
            .map_err(|error| match error {
                GraphDbError::Conflict => VectorGenerationStoreErrorV1::ConcurrentMutation,
                other => map_graph_error(other),
            })
    }
}

fn native_state_watermark_from_encoded(
    encoded: &NativeGraphStateV1,
) -> Result<(GraphWatermark, tracedecay_domain::ManifestDigest), VectorGenerationStoreErrorV1> {
    let state_digest = canonical_sha256(&(
        SEMANTIC_VECTOR_GRAPH_STATE_DIGEST_DOMAIN,
        encoded.revision,
        &encoded.entities,
        &encoded.relations,
    ))
    .map_err(storage_error)?;
    let watermark = GraphWatermark::new(format!(
        "semantic-vector:{}:{}",
        encoded.revision,
        state_digest.as_str()
    ))
    .map_err(map_graph_error)?;
    Ok((watermark, state_digest))
}

pub(super) fn measured_resident_bytes(
    rows: &[ResidentVectorRowV1],
) -> Result<u64, VectorGenerationStoreErrorV1> {
    rows.iter().try_fold(0_u64, |total, row| {
        let vector_bytes = u64::try_from(row.values.len())
            .map_err(storage_error)?
            .checked_mul(u64::try_from(size_of::<f32>()).map_err(storage_error)?)
            .ok_or_else(resident_size_overflow)?;
        let row_bytes = u64::try_from(size_of::<ResidentVectorRowV1>())
            .map_err(storage_error)?
            .checked_add(u64::try_from(row.chunk_id.to_string().len()).map_err(storage_error)?)
            .and_then(|bytes| bytes.checked_add(vector_bytes))
            .ok_or_else(resident_size_overflow)?;
        total
            .checked_add(row_bytes)
            .ok_or_else(resident_size_overflow)
    })
}

pub(super) fn resident_size_overflow() -> VectorGenerationStoreErrorV1 {
    VectorGenerationStoreErrorV1::Corrupt("semantic resident vector size exceeds u64".to_owned())
}

pub(super) fn graph_namespace() -> Result<GraphNamespace, VectorGenerationStoreErrorV1> {
    GraphNamespace::new(SEMANTIC_VECTOR_GRAPH_PROJECTION).map_err(map_graph_error)
}

pub(super) fn graph_projection() -> Result<GraphProjectionId, VectorGenerationStoreErrorV1> {
    GraphProjectionId::new(SEMANTIC_VECTOR_GRAPH_PROJECTION).map_err(map_graph_error)
}

fn graph_label(value: &str) -> Result<GraphLabel, VectorGenerationStoreErrorV1> {
    GraphLabel::new(value).map_err(map_graph_error)
}

pub(super) fn generation_label(
    generation: &VectorGenerationIdV1,
) -> Result<GraphLabel, VectorGenerationStoreErrorV1> {
    graph_label(&format!(
        "semantic-vector-generation:{}",
        generation.as_digest().as_str()
    ))
}

pub(super) fn search_vector_property(
    _generation: &VectorGenerationIdV1,
) -> Result<GraphPropertyName, VectorGenerationStoreErrorV1> {
    property_name(VECTOR_PROPERTY)
}

fn property_name(value: &str) -> Result<GraphPropertyName, VectorGenerationStoreErrorV1> {
    GraphPropertyName::new(value).map_err(map_graph_error)
}

fn required_property<'a>(
    entity: &'a GraphEntity,
    name: &str,
) -> Result<&'a GraphProperty, VectorGenerationStoreErrorV1> {
    entity.properties.get(&property_name(name)?).ok_or_else(|| {
        VectorGenerationStoreErrorV1::Corrupt(format!(
            "semantic vector graph entity {} is missing property {name}",
            entity.identity
        ))
    })
}

pub(super) fn required_string<'a>(
    entity: &'a GraphEntity,
    name: &str,
) -> Result<&'a str, VectorGenerationStoreErrorV1> {
    match required_property(entity, name)? {
        GraphProperty::String(value) => Ok(value),
        _ => Err(VectorGenerationStoreErrorV1::Corrupt(format!(
            "semantic vector graph entity {} property {name} has the wrong type",
            entity.identity
        ))),
    }
}

pub(super) const fn vector_metric(metric: EmbeddingMetricV1) -> VectorMetric {
    match metric {
        EmbeddingMetricV1::Cosine => VectorMetric::Cosine,
        EmbeddingMetricV1::DotProduct => VectorMetric::DotProduct,
        EmbeddingMetricV1::EuclideanL2 => VectorMetric::Euclidean,
    }
}

pub(super) fn normalized_vector_score(distance: f64) -> f64 {
    if distance <= 0.0 {
        1.0
    } else {
        1.0 / (1.0 + distance)
    }
}

pub(super) fn check_cancelled(
    cancellation: &dyn GraphCancellation,
) -> Result<(), VectorGenerationStoreErrorV1> {
    if cancellation.is_cancelled() {
        Err(VectorGenerationStoreErrorV1::Cancelled)
    } else {
        Ok(())
    }
}

pub(super) fn map_graph_error(error: GraphDbError) -> VectorGenerationStoreErrorV1 {
    match error {
        GraphDbError::Cancelled => VectorGenerationStoreErrorV1::Cancelled,
        GraphDbError::Conflict => VectorGenerationStoreErrorV1::ConcurrentMutation,
        GraphDbError::ResetRequired { message } => {
            VectorGenerationStoreErrorV1::ResetRequired(message)
        }
        GraphDbError::Corrupt { message } => VectorGenerationStoreErrorV1::Corrupt(message),
        GraphDbError::Unavailable { message } => VectorGenerationStoreErrorV1::Unavailable(message),
        GraphDbError::InvalidRequest { message } => {
            VectorGenerationStoreErrorV1::InvalidPlan(message)
        }
        GraphDbError::DurabilityUncertain { message } => {
            VectorGenerationStoreErrorV1::DurabilityUncertain(message)
        }
        GraphDbError::BudgetExhausted => VectorGenerationStoreErrorV1::Unavailable(
            "semantic vector graph read budget is exhausted".to_owned(),
        ),
        GraphDbError::DeadlineExceeded => VectorGenerationStoreErrorV1::DeadlineExceeded,
        GraphDbError::Closed => {
            VectorGenerationStoreErrorV1::Unavailable("graph database is closed".to_owned())
        }
    }
}

pub(super) fn storage_error(error: impl std::fmt::Display) -> VectorGenerationStoreErrorV1 {
    VectorGenerationStoreErrorV1::Corrupt(error.to_string())
}
