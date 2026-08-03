use std::fmt;
use std::sync::Arc;

use grafeo_core::index::vector::DistanceMetric;
use grafeo_engine::GrafeoDB;
use serde::{Deserialize, Serialize};

use crate::runtime::{ENTITY_LABEL, load_entities, vector_property_key};
use crate::{
    GraphCancellation, GraphDbError, GraphEntityId, GraphNamespace, GraphProperty,
    GraphPropertyName,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum VectorMetric {
    Cosine,
    DotProduct,
    Euclidean,
}

impl VectorMetric {
    pub fn parse(value: &str) -> Result<Self, GraphDbError> {
        match value {
            "cosine" => Ok(Self::Cosine),
            "dot_product" => Ok(Self::DotProduct),
            "euclidean" => Ok(Self::Euclidean),
            unsupported => Err(GraphDbError::invalid(format!(
                "unsupported vector metric `{unsupported}`"
            ))),
        }
    }

    pub(crate) const fn into_grafeo(self) -> DistanceMetric {
        match self {
            Self::Cosine => DistanceMetric::Cosine,
            Self::DotProduct => DistanceMetric::DotProduct,
            Self::Euclidean => DistanceMetric::Euclidean,
        }
    }
}

#[derive(Clone)]
pub struct VectorSearchRequest {
    pub namespace: GraphNamespace,
    pub property: GraphPropertyName,
    pub query: Vec<f32>,
    pub dimension: usize,
    pub metric: VectorMetric,
    pub limit: usize,
    pub cancellation: Arc<dyn GraphCancellation>,
}

impl fmt::Debug for VectorSearchRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VectorSearchRequest")
            .field("namespace", &self.namespace)
            .field("property", &self.property)
            .field("dimension", &self.dimension)
            .field("metric", &self.metric)
            .field("limit", &self.limit)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct VectorMatch {
    pub entity: GraphEntityId,
    pub distance: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct VectorSearchResult {
    pub matches: Vec<VectorMatch>,
}

pub(crate) fn vector_search(
    database: &GrafeoDB,
    request: VectorSearchRequest,
) -> Result<VectorSearchResult, GraphDbError> {
    validate_request(&request)?;
    let entities = load_entities(database)?;
    let store = database.graph_store();
    let key = vector_property_key(&request.property);
    let candidates = store.vector_search(
        Some(ENTITY_LABEL),
        &key,
        &request.query,
        store.node_count(),
        request.metric.into_grafeo(),
    );
    if request.cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }

    let mut matches = Vec::new();
    for (node_id, distance) in candidates {
        if request.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let Some(stored) = entities.values().find_map(|(candidate, stored)| {
            (*candidate == node_id && stored.namespace == request.namespace).then_some(stored)
        }) else {
            continue;
        };
        let Some(GraphProperty::Vector(vector)) = stored.entity.properties.get(&request.property)
        else {
            continue;
        };
        if vector.dimension == request.dimension
            && vector.metric == request.metric
            && distance.is_finite()
        {
            matches.push(VectorMatch {
                entity: stored.entity.identity.clone(),
                distance: normalize_distance(distance),
            });
        }
    }
    matches.sort_by(|left, right| {
        left.distance
            .total_cmp(&right.distance)
            .then_with(|| left.entity.cmp(&right.entity))
    });
    matches.truncate(request.limit);
    Ok(VectorSearchResult { matches })
}

fn normalize_distance(distance: f64) -> f64 {
    if distance.abs() <= f64::from(f32::EPSILON) {
        0.0
    } else {
        distance
    }
}

fn validate_request(request: &VectorSearchRequest) -> Result<(), GraphDbError> {
    if request.cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }
    if request.dimension == 0 || request.query.len() != request.dimension {
        return Err(GraphDbError::invalid(
            "query vector length must match its non-zero declared dimension",
        ));
    }
    if request.query.iter().any(|value| !value.is_finite()) {
        return Err(GraphDbError::invalid(
            "query vector values must all be finite",
        ));
    }
    if request.limit == 0 {
        return Err(GraphDbError::invalid(
            "vector result limit must be greater than zero",
        ));
    }
    Ok(())
}
