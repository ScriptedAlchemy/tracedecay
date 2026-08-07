use std::mem::size_of;

use tracedecay_domain::{EmbeddingMetricV1, VectorGenerationIdV1};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphEntity, GraphLabel, GraphProperty, GraphPropertyName,
    VectorMetric,
};

use super::super::VectorGenerationStoreErrorV1;
use super::ResidentVectorRowV1;

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

pub(super) fn generation_label(
    generation: &VectorGenerationIdV1,
) -> Result<GraphLabel, VectorGenerationStoreErrorV1> {
    tracedecay_graph_db::semantic_vector_native::generation_label(generation.as_digest().as_str())
        .map_err(map_graph_error)
}

pub(super) fn search_vector_property(
    generation: &VectorGenerationIdV1,
) -> Result<GraphPropertyName, VectorGenerationStoreErrorV1> {
    tracedecay_graph_db::semantic_vector_native::vector_property(generation.as_digest().as_str())
        .map_err(map_graph_error)
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
        GraphDbError::ProjectionMismatch { message, .. }
        | GraphDbError::GenerationMismatch { message, .. } => {
            VectorGenerationStoreErrorV1::ResetRequired(message)
        }
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
