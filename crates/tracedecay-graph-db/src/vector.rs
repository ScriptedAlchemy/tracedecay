use std::fmt;
use std::sync::Arc;

use grafeo_common::types::{NodeId, PropertyKey, Value};
use grafeo_common::utils::hash::FxHashMap;
use grafeo_engine::GrafeoDB;
use serde::{Deserialize, Serialize};

use crate::schema::{
    ENTITY_ID_PROPERTY, NAMESPACE_PROPERTY, PROJECTION_PROPERTY, entity_projection_label,
    required_string, vector_property_key,
};
use crate::{
    GraphCancellation, GraphDbError, GraphEntityId, GraphNamespace, GraphProjectionId,
    GraphPropertyName,
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

    #[hotpath::skip]
    pub(crate) const fn engine_name(self) -> &'static str {
        match self {
            Self::Cosine => "cosine",
            Self::DotProduct => "dot_product",
            Self::Euclidean => "euclidean",
        }
    }

    #[hotpath::skip]
    pub(crate) const fn storage_tag(self) -> &'static str {
        match self {
            Self::Cosine => "cos",
            Self::DotProduct => "dot",
            Self::Euclidean => "l2",
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

/// What the store can currently answer for one exact-projection index.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphVectorIndexStatus {
    /// The index exists and covers `vectors` rows. Searches can be served;
    /// callers that require complete coverage compare `vectors` against the
    /// row set they serve.
    Available { vectors: usize },
    /// No index exists for this label and property. A build is needed and
    /// nothing is being answered wrongly in the meantime: `vector_search`
    /// reports the index as absent rather than returning an empty result.
    Missing,
    /// The index exists and covers nothing.
    ///
    /// This is the failure worth having its own name. Grafeo's
    /// `create_vector_index` scans `nodes_by_label` and succeeds over an
    /// empty match, leaving an index registered with zero vectors -
    /// after which `has_vector_index` answers `true` and every search
    /// returns no matches while reporting an index exists. A caller that
    /// only distinguished present from absent would read that as a
    /// working index and never rebuild.
    ///
    /// Reachable whenever an index is built before the rows it covers
    /// land, or over a store whose rows moved out from under the label
    /// the index was declared on.
    Stale,
}

impl GraphVectorIndexStatus {
    /// Whether a caller should build or rebuild before trusting searches.
    #[must_use]
    #[hotpath::skip]
    pub const fn needs_build(self) -> bool {
        matches!(self, Self::Missing | Self::Stale)
    }
}

/// The store's own census of one vector index.
///
/// Read through `GrafeoDB::memory_usage`, which is the only public view
/// into an index's coverage: `has_vector_index` answers a bare bool and
/// cannot tell a populated index from an empty one.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct VectorIndexCensus {
    pub(crate) vectors: usize,
    pub(crate) bytes: usize,
}

/// Censuses the index registered for `label`/`property`, if any.
pub(crate) fn vector_index_census(
    database: &GrafeoDB,
    label: &str,
    property: &str,
) -> Option<VectorIndexCensus> {
    // Grafeo keys its per-index memory entries by "label:property", the
    // same key `add_vector_index` files them under.
    let key = format!("{label}:{property}");
    database
        .memory_usage()
        .indexes
        .vector_indexes
        .into_iter()
        .find(|entry| entry.name == key)
        .map(|entry| VectorIndexCensus {
            vectors: entry.item_count,
            bytes: entry.bytes,
        })
}

/// Classifies one index from the store's census, and reports its size.
pub(crate) fn classify_vector_index(
    database: &GrafeoDB,
    label: &str,
    property: &str,
) -> GraphVectorIndexStatus {
    if !database.graph_store().has_vector_index(label, property) {
        return GraphVectorIndexStatus::Missing;
    }
    match vector_index_census(database, label, property) {
        Some(census) if census.vectors > 0 => {
            crate::hotpath_observe::record_vector_index_size(census.vectors, census.bytes);
            GraphVectorIndexStatus::Available {
                vectors: census.vectors,
            }
        }
        // Registered but covering nothing, or registered somewhere the
        // census cannot see it. Either way a search would answer empty
        // while claiming an index exists, so say so.
        _ => GraphVectorIndexStatus::Stale,
    }
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
    request: VectorSearchRequest,
) -> Result<VectorSearchResult, GraphDbError> {
    validate_request(&request)?;
    let key = vector_property_key(&request.property, request.dimension, request.metric);
    let label = native_vector_label(&request.namespace, &request.projection);
    let ef = request
        .limit
        .saturating_mul(4)
        .clamp(64, MAX_VECTOR_SEARCH_EF);
    let candidates = database
        .vector_search(&label, &key, &request.query, request.limit, Some(ef), None)
        .map_err(|error| GraphDbError::unavailable(error.to_string()))?;
    if request.cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }
    hotpath::gauge!("graph_db.search.vector.candidates").set(candidates.len());

    // The index answers with node ids; confirming each one still belongs to
    // the requested projection and still carries the searched vector needs
    // exactly four scalar columns. Read them in one projected batch instead
    // of hydrating every candidate through the identity index and decoding
    // its full property set (including the vector itself) per row.
    let node_ids = candidates
        .iter()
        .map(|(node_id, _)| *node_id)
        .collect::<Vec<NodeId>>();
    let columns = CandidateColumns::new(&key);
    let rows = hotpath::measure_block!(
        "graph_db.search.vector.candidate_columns",
        database
            .graph_store()
            .get_nodes_properties_selective_batch(&node_ids, columns.keys())
    );
    if rows.len() != node_ids.len() {
        return Err(GraphDbError::Corrupt {
            message: "vector candidate column batch does not cover every candidate".to_owned(),
        });
    }

    let mut matches = Vec::with_capacity(candidates.len().min(request.limit));
    for ((_, distance), row) in candidates.into_iter().zip(rows) {
        if request.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let Some(entity) = columns.admit(&row, &request)? else {
            continue;
        };
        if distance.is_finite() {
            matches.push(VectorMatch {
                entity,
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
    hotpath::gauge!("graph_db.search.vector.results").set(matches.len());
    Ok(VectorSearchResult { matches })
}

pub(crate) fn native_vector_label(
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
) -> String {
    entity_projection_label(namespace, projection)
}

/// The scalar columns one candidate must present to count as a match:
/// owner namespace and projection, entity identity, and the searched
/// vector under its exact `name_dimension_metric` key.
struct CandidateColumns {
    keys: [PropertyKey; 4],
}

impl CandidateColumns {
    fn new(vector_key: &str) -> Self {
        Self {
            keys: [
                PropertyKey::new(NAMESPACE_PROPERTY),
                PropertyKey::new(PROJECTION_PROPERTY),
                PropertyKey::new(ENTITY_ID_PROPERTY),
                PropertyKey::new(vector_key),
            ],
        }
    }

    fn keys(&self) -> &[PropertyKey] {
        &self.keys
    }

    /// Resolves one candidate row to its entity identity, or `None` when the
    /// row belongs to another projection or no longer carries the searched
    /// vector. A row with no identity columns at all is the node the index
    /// pointed at having vanished from under it, which is store corruption
    /// rather than a filtered-out neighbour.
    fn admit(
        &self,
        row: &FxHashMap<PropertyKey, Value>,
        request: &VectorSearchRequest,
    ) -> Result<Option<GraphEntityId>, GraphDbError> {
        let [namespace_key, projection_key, entity_key, vector_key] = &self.keys;
        if row.is_empty() {
            return Err(GraphDbError::Corrupt {
                message: "entity node is unreadable".to_owned(),
            });
        }
        let namespace = required_string(row.get(namespace_key), "entity namespace")?;
        let projection = required_string(row.get(projection_key), "entity projection")?;
        if namespace != request.namespace.as_str() || projection != request.projection.as_str() {
            return Ok(None);
        }
        // The property key already binds name, dimension, and metric; the
        // stored length is the one thing the key cannot vouch for. The HNSW
        // topology can still return a node after that exact vector scalar was
        // removed or superseded, so any non-vector value is a stale candidate
        // rather than a search failure.
        match row.get(vector_key) {
            Some(Value::Vector(values)) if values.len() == request.dimension => {}
            Some(Value::Vector(_)) => {
                return Err(GraphDbError::Corrupt {
                    message: "native vector property does not match its declared dimension"
                        .to_owned(),
                });
            }
            _ => return Ok(None),
        }
        let identity = GraphEntityId::new(required_string(row.get(entity_key), "entity identity")?)
            .map_err(|error| GraphDbError::Corrupt {
                message: format!("invalid persisted entity identity: {error}"),
            })?;
        Ok(Some(identity))
    }
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
