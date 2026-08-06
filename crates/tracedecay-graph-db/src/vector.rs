use std::fmt;
use std::sync::Arc;

use grafeo_engine::GrafeoDB;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::runtime::vector_property_key;
use crate::state::StateCache;
use crate::{
    GraphCancellation, GraphDbError, GraphEntityId, GraphNamespace, GraphProjectionId,
    GraphProperty, GraphPropertyName,
};

pub const MAX_VECTOR_SEARCH_LIMIT: usize = 4_096;
const MAX_VECTOR_SEARCH_EF: usize = MAX_VECTOR_SEARCH_LIMIT * 4;

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

    pub(crate) const fn storage_tag(self) -> &'static str {
        match self {
            Self::Cosine => "cos",
            Self::DotProduct => "dot",
            Self::Euclidean => "l2",
        }
    }

    pub(crate) const fn engine_name(self) -> &'static str {
        match self {
            Self::Cosine => "cosine",
            Self::DotProduct => "dot_product",
            Self::Euclidean => "euclidean",
        }
    }
}

#[derive(Clone)]
pub struct VectorSearchRequest {
    pub namespace: GraphNamespace,
    pub projection: GraphProjectionId,
    pub property: GraphPropertyName,
    pub query: Vec<f32>,
    pub dimension: usize,
    pub metric: VectorMetric,
    pub limit: usize,
    pub cancellation: Arc<dyn GraphCancellation>,
}

#[derive(Clone)]
pub struct GraphVectorIndexRequest {
    pub namespace: GraphNamespace,
    pub projection: GraphProjectionId,
    pub property: GraphPropertyName,
    pub dimension: usize,
    pub metric: VectorMetric,
    pub cancellation: Arc<dyn GraphCancellation>,
}

impl GraphVectorIndexRequest {
    pub(crate) fn validate(&self) -> Result<(), GraphDbError> {
        if self.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        if self.dimension == 0 {
            return Err(GraphDbError::invalid(
                "vector index dimension must be greater than zero",
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for GraphVectorIndexRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GraphVectorIndexRequest")
            .field("namespace", &self.namespace)
            .field("projection", &self.projection)
            .field("property", &self.property)
            .field("dimension", &self.dimension)
            .field("metric", &self.metric)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphVectorIndexStatus {
    Available,
    Missing,
}

impl fmt::Debug for VectorSearchRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VectorSearchRequest")
            .field("namespace", &self.namespace)
            .field("projection", &self.projection)
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
    state: &StateCache,
    request: VectorSearchRequest,
) -> Result<VectorSearchResult, GraphDbError> {
    validate_request(&request)?;
    let key = vector_property_key(&request.property, request.dimension, request.metric);
    let label = native_vector_label(
        &request.namespace,
        &request.projection,
        &request.property,
        request.dimension,
        request.metric,
    );
    let ef = request
        .limit
        .saturating_mul(4)
        .max(64)
        .min(MAX_VECTOR_SEARCH_EF);
    let candidates = database
        .vector_search(&label, &key, &request.query, request.limit, Some(ef), None)
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    if request.cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }

    let mut matches = Vec::new();
    for (node_id, distance) in candidates {
        if request.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let Some(stored) = state.entity_by_node(node_id) else {
            continue;
        };
        if stored.namespace != request.namespace || stored.projection != request.projection {
            continue;
        }
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
                distance: normalize_distance(f64::from(distance)),
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

pub(crate) fn native_vector_label(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    property: &GraphPropertyName,
    dimension: usize,
    metric: VectorMetric,
) -> String {
    let mut digest = Sha256::new();
    for component in [
        namespace.as_str(),
        projection.as_str(),
        property.as_str(),
        metric.storage_tag(),
    ] {
        digest.update(component.len().to_le_bytes());
        digest.update(component.as_bytes());
    }
    digest.update(dimension.to_le_bytes());
    format!(
        "__tracedecay_graph_db_vector_{}",
        hex::encode(digest.finalize())
    )
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
    if request.limit > MAX_VECTOR_SEARCH_LIMIT {
        return Err(GraphDbError::invalid(format!(
            "vector result limit exceeds {MAX_VECTOR_SEARCH_LIMIT}"
        )));
    }
    Ok(())
}
