use tracedecay_domain::{EmbeddingMetricV1, VectorGenerationIdV1};
use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphLabel, GraphPropertyName, VectorMetric,
};

use super::super::VectorGenerationStoreErrorV1;

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

pub(super) const fn vector_metric(metric: EmbeddingMetricV1) -> VectorMetric {
    match metric {
        EmbeddingMetricV1::Cosine => VectorMetric::Cosine,
        EmbeddingMetricV1::DotProduct => VectorMetric::DotProduct,
        EmbeddingMetricV1::EuclideanL2 => VectorMetric::Euclidean,
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
