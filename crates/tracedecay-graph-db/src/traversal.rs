use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::Arc;

use grafeo_adapters::plugins::algorithms::{Control, TraversalEvent, bfs_with_visitor};
use grafeo_common::types::{EdgeId, NodeId, Value};
use grafeo_core::graph::{
    Direction, GraphProjection, GraphStore, GraphStoreSearch, ProjectionSpec,
};
use grafeo_engine::GrafeoDB;

use crate::adjacency_id_index::{AdjacencyIdIndexCache, AdjacencyIndexKey, page_ids};
use crate::epoch_cache::LabelKeyCache;
use crate::schema::{
    ENTITY_ID_PROPERTY, ENTITY_KEY_PROPERTY, ENTITY_LABEL, NAMESPACE_PROPERTY, PROJECTION_PROPERTY,
    RELATION_FROM_PROPERTY, RELATION_ID_PROPERTY, RELATION_KIND_PROPERTY, RELATION_TO_PROPERTY,
    decode_entity, decode_graph_properties, decode_relation_identity, entity_key_value,
    entity_projection_label, label_keys, relation_kind_from_type, relation_type_for_kind,
};
use crate::{
    GraphBudgetKind, GraphCancellation, GraphDbError, GraphEntity, GraphEntityId, GraphNamespace,
    GraphProjectionId, GraphRelation, GraphRelationId, GraphRelationKind, GraphSnapshot,
    VectorSearchRequest, VectorSearchResult,
};

const MAX_BATCH_TRAVERSAL_STARTS: usize = 100_000;

fn read_budget(limit: usize) -> GraphDbError {
    GraphDbError::budget_exhausted_count(GraphBudgetKind::Read, limit)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphTraversalDirection {
    Outgoing,
    Incoming,
    Both,
}

/// How a bulk fan-out behaves when the next relation would exceed `max_relations`.
///
/// Complete-adjacency tools must [`Self::Refuse`] so a truncated page cannot be
/// mistaken for the full neighborhood. Page-shaped consumers (context related
/// symbols) use [`Self::Truncate`]: they already keep a small prefix.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelationFanoutOverflow {
    Refuse,
    Truncate,
}

#[derive(Clone)]
pub struct TraversalRequest {
    pub namespace: GraphNamespace,
    pub start: GraphEntityId,
    pub relation_kinds: BTreeSet<GraphRelationKind>,
    pub direction: GraphTraversalDirection,
    pub max_depth: usize,
    pub max_visits: usize,
    pub max_results: usize,
    pub cancellation: Arc<dyn GraphCancellation>,
}

impl fmt::Debug for TraversalRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TraversalRequest")
            .field("namespace", &self.namespace)
            .field("start", &self.start)
            .field("relation_kinds", &self.relation_kinds)
            .field("direction", &self.direction)
            .field("max_depth", &self.max_depth)
            .field("max_visits", &self.max_visits)
            .field("max_results", &self.max_results)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraversalVisit {
    pub entity: GraphEntityId,
    pub depth: usize,
    pub via_relation: Option<GraphRelationId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TraversalResult {
    pub visits: Vec<TraversalVisit>,
}

#[derive(Clone, Debug)]
pub struct GraphRelationTarget {
    pub relation: GraphRelation,
    pub target: GraphEntity,
}

impl GraphSnapshot {
    pub fn traverse(&self, request: TraversalRequest) -> Result<TraversalResult, GraphDbError> {
        let result = self.database.traverse(request)?;
        crate::hotpath_observe::record_hydration_source(
            crate::hotpath_observe::HydrationSource::Snapshot,
        );
        Ok(result)
    }

    /// Reads bounded outgoing relations while retaining this snapshot's
    /// projection lease, so a complete traversal cannot mix two replacement
    /// generations.
    pub fn outgoing_relations(
        &self,
        namespace: &GraphNamespace,
        starts: &[GraphEntityId],
        relation_kinds: &BTreeSet<GraphRelationKind>,
        max_relations: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Vec<Vec<GraphRelation>>, GraphDbError> {
        self.database.outgoing_relations(
            namespace,
            starts,
            relation_kinds,
            max_relations,
            cancellation,
        )
    }

    pub fn vector_search(
        &self,
        request: VectorSearchRequest,
    ) -> Result<VectorSearchResult, GraphDbError> {
        self.database.vector_search(request)
    }
}

pub(crate) fn traverse(
    database: &GrafeoDB,
    request: TraversalRequest,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
) -> Result<TraversalResult, GraphDbError> {
    validate_request(&request)?;
    if request.max_results == 0 {
        return Ok(TraversalResult { visits: Vec::new() });
    }

    let store = database.graph_store();
    let start = node_for_entity(store.as_ref(), &request.namespace, &request.start)?;
    let projected = relation_projection(store, &request.relation_kinds);
    match request.direction {
        GraphTraversalDirection::Outgoing => {
            native_outgoing_traversal(&projected, start, &request, ensure_projection_readable)
        }
        GraphTraversalDirection::Incoming | GraphTraversalDirection::Both => {
            directional_traversal(&projected, start, &request, ensure_projection_readable)
        }
    }
}

pub(crate) fn outgoing_relation_ids(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    starts: &[GraphEntityId],
    relation_kinds: &BTreeSet<GraphRelationKind>,
    max_relations: usize,
    cancellation: &dyn GraphCancellation,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
    label_keys_cache: &LabelKeyCache,
    adjacency_ids: &AdjacencyIdIndexCache,
) -> Result<Vec<Vec<GraphRelationId>>, GraphDbError> {
    directed_relation_ids(
        database,
        namespace,
        starts,
        relation_kinds,
        max_relations,
        false,
        cancellation,
        ensure_projection_readable,
        label_keys_cache,
        adjacency_ids,
        RelationFanoutOverflow::Refuse,
        None,
    )
}

pub(crate) fn outgoing_relation_ids_page(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    starts: &[GraphEntityId],
    relation_kinds: &BTreeSet<GraphRelationKind>,
    after: Option<&GraphRelationId>,
    limit: usize,
    cancellation: &dyn GraphCancellation,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
    label_keys_cache: &LabelKeyCache,
    adjacency_ids: &AdjacencyIdIndexCache,
) -> Result<Vec<Vec<GraphRelationId>>, GraphDbError> {
    directed_relation_ids(
        database,
        namespace,
        starts,
        relation_kinds,
        limit,
        false,
        cancellation,
        ensure_projection_readable,
        label_keys_cache,
        adjacency_ids,
        RelationFanoutOverflow::Truncate,
        after,
    )
}

/// Bulk kind-filtered incoming fan-out: the exact counterpart of
/// [`outgoing_relation_ids`], carrying the same batch, cancellation, dedupe,
/// and `max_relations` budget semantics.
///
/// Plan 39 G7b needs this so an interactive caller/impact read can resolve
/// reverse adjacency through the graph store instead of a SQL `edges` join.
/// Only the traversal direction differs from the outgoing form, so both
/// delegate to [`directed_relations`].
pub(crate) fn incoming_relation_ids(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    starts: &[GraphEntityId],
    relation_kinds: &BTreeSet<GraphRelationKind>,
    max_relations: usize,
    cancellation: &dyn GraphCancellation,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
    label_keys_cache: &LabelKeyCache,
    adjacency_ids: &AdjacencyIdIndexCache,
) -> Result<Vec<Vec<GraphRelationId>>, GraphDbError> {
    directed_relation_ids(
        database,
        namespace,
        starts,
        relation_kinds,
        max_relations,
        true,
        cancellation,
        ensure_projection_readable,
        label_keys_cache,
        adjacency_ids,
        RelationFanoutOverflow::Refuse,
        None,
    )
}

pub(crate) fn incoming_relation_ids_page(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    starts: &[GraphEntityId],
    relation_kinds: &BTreeSet<GraphRelationKind>,
    after: Option<&GraphRelationId>,
    limit: usize,
    cancellation: &dyn GraphCancellation,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
    label_keys_cache: &LabelKeyCache,
    adjacency_ids: &AdjacencyIdIndexCache,
) -> Result<Vec<Vec<GraphRelationId>>, GraphDbError> {
    directed_relation_ids(
        database,
        namespace,
        starts,
        relation_kinds,
        limit,
        true,
        cancellation,
        ensure_projection_readable,
        label_keys_cache,
        adjacency_ids,
        RelationFanoutOverflow::Truncate,
        after,
    )
}

pub(crate) fn outgoing_relations(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    starts: &[GraphEntityId],
    relation_kinds: &BTreeSet<GraphRelationKind>,
    max_relations: usize,
    cancellation: &dyn GraphCancellation,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
) -> Result<Vec<Vec<GraphRelation>>, GraphDbError> {
    directed_relations(
        database,
        namespace,
        starts,
        relation_kinds,
        max_relations,
        Direction::Outgoing,
        cancellation,
        ensure_projection_readable,
        RelationFanoutOverflow::Refuse,
    )
}

pub(crate) fn outgoing_relations_truncated(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    starts: &[GraphEntityId],
    relation_kinds: &BTreeSet<GraphRelationKind>,
    max_relations: usize,
    cancellation: &dyn GraphCancellation,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
) -> Result<Vec<Vec<GraphRelation>>, GraphDbError> {
    directed_relations(
        database,
        namespace,
        starts,
        relation_kinds,
        max_relations,
        Direction::Outgoing,
        cancellation,
        ensure_projection_readable,
        RelationFanoutOverflow::Truncate,
    )
}

pub(crate) fn outgoing_relation_targets(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    starts: &[GraphEntityId],
    relation_kinds: &BTreeSet<GraphRelationKind>,
    max_relations: usize,
    cancellation: &dyn GraphCancellation,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
) -> Result<Vec<Vec<GraphRelationTarget>>, GraphDbError> {
    check_batch_request(starts, cancellation)?;
    let store = database.graph_store();
    let projected = relation_projection(Arc::clone(&store), relation_kinds);
    let mut admitted = 0_usize;
    let mut results = Vec::with_capacity(starts.len());
    for start in starts {
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let Some(node) = optional_node_for_entity(store.as_ref(), namespace, start)? else {
            results.push(Vec::new());
            continue;
        };
        let mut targets = Vec::new();
        for (neighbor, edge) in projected.edges_from(node, Direction::Outgoing) {
            if cancellation.is_cancelled() {
                return Err(GraphDbError::Cancelled);
            }
            let target = store
                .get_node(neighbor)
                .ok_or_else(|| GraphDbError::Corrupt {
                    message: "outgoing relation target is missing".to_owned(),
                })?;
            let target = decode_entity(&target)?;
            let relation = relation_for_edge(
                &projected,
                edge,
                namespace,
                ensure_projection_readable,
                RelationEndpointCheck::Expected {
                    from: start,
                    to: &target.identity,
                },
            )?;
            targets.push(GraphRelationTarget { relation, target });
        }
        targets.sort_by(|left, right| left.relation.identity.cmp(&right.relation.identity));
        targets.dedup_by(|left, right| left.relation.identity == right.relation.identity);
        admitted = admitted
            .checked_add(targets.len())
            .ok_or_else(|| read_budget(max_relations))?;
        if admitted > max_relations {
            return Err(read_budget(max_relations));
        }
        results.push(targets);
    }
    Ok(results)
}

pub(crate) fn visit_outgoing_relation_targets(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    start: &GraphEntityId,
    relation_kinds: &BTreeSet<GraphRelationKind>,
    cancellation: &dyn GraphCancellation,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
    visitor: &mut dyn FnMut(GraphRelationTarget),
) -> Result<usize, GraphDbError> {
    if cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }
    let store = database.graph_store();
    let projected = relation_projection(Arc::clone(&store), relation_kinds);
    let Some(node) = optional_node_for_entity(store.as_ref(), namespace, start)? else {
        return Ok(0);
    };
    let mut visited = 0_usize;
    for (neighbor, edge) in projected.edges_from(node, Direction::Outgoing) {
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let target = store
            .get_node(neighbor)
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "outgoing relation target is missing".to_owned(),
            })?;
        let target = decode_entity(&target)?;
        let relation = relation_for_edge(
            &projected,
            edge,
            namespace,
            ensure_projection_readable,
            RelationEndpointCheck::Expected {
                from: start,
                to: &target.identity,
            },
        )?;
        visitor(GraphRelationTarget { relation, target });
        visited = visited
            .checked_add(1)
            .ok_or_else(|| GraphDbError::Corrupt {
                message: "outgoing relation target count overflowed".to_owned(),
            })?;
    }
    Ok(visited)
}

/// Bulk kind-filtered incoming fan-out. See [`incoming_relation_ids`].
pub(crate) fn incoming_relations(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    starts: &[GraphEntityId],
    relation_kinds: &BTreeSet<GraphRelationKind>,
    max_relations: usize,
    cancellation: &dyn GraphCancellation,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
) -> Result<Vec<Vec<GraphRelation>>, GraphDbError> {
    directed_relations(
        database,
        namespace,
        starts,
        relation_kinds,
        max_relations,
        Direction::Incoming,
        cancellation,
        ensure_projection_readable,
        RelationFanoutOverflow::Refuse,
    )
}

pub(crate) fn incoming_relations_truncated(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    starts: &[GraphEntityId],
    relation_kinds: &BTreeSet<GraphRelationKind>,
    max_relations: usize,
    cancellation: &dyn GraphCancellation,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
) -> Result<Vec<Vec<GraphRelation>>, GraphDbError> {
    directed_relations(
        database,
        namespace,
        starts,
        relation_kinds,
        max_relations,
        Direction::Incoming,
        cancellation,
        ensure_projection_readable,
        RelationFanoutOverflow::Truncate,
    )
}

#[allow(clippy::too_many_arguments)]
fn directed_relation_ids(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    starts: &[GraphEntityId],
    relation_kinds: &BTreeSet<GraphRelationKind>,
    max_relations: usize,
    incoming: bool,
    cancellation: &dyn GraphCancellation,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
    label_keys_cache: &LabelKeyCache,
    adjacency_ids: &AdjacencyIdIndexCache,
    overflow: RelationFanoutOverflow,
    after: Option<&GraphRelationId>,
) -> Result<Vec<Vec<GraphRelationId>>, GraphDbError> {
    check_batch_request(starts, cancellation)?;
    let mut admitted = 0_usize;
    let mut results = Vec::with_capacity(starts.len());
    let mut truncated = false;
    for start in starts {
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        if truncated {
            results.push(Vec::new());
            continue;
        }
        let ids = ordered_relation_ids(
            database,
            namespace,
            start,
            relation_kinds,
            incoming,
            cancellation,
            ensure_projection_readable,
            label_keys_cache,
            adjacency_ids,
        )?;
        let page = match after {
            Some(_) | None if overflow == RelationFanoutOverflow::Truncate => {
                page_ids(&ids, after, max_relations)
            }
            _ => ids.iter().cloned().collect::<Vec<_>>(),
        };
        match overflow {
            RelationFanoutOverflow::Refuse => {
                admitted = admitted
                    .checked_add(page.len())
                    .ok_or_else(|| read_budget(max_relations))?;
                if admitted > max_relations {
                    return Err(read_budget(max_relations));
                }
                results.push(page);
            }
            RelationFanoutOverflow::Truncate => {
                let remaining = max_relations.saturating_sub(admitted);
                let mut page = page;
                if page.len() > remaining {
                    page.truncate(remaining);
                    truncated = true;
                }
                admitted = admitted.saturating_add(page.len());
                results.push(page);
            }
        }
    }
    Ok(results)
}

#[allow(clippy::too_many_arguments)]
fn ordered_relation_ids(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    start: &GraphEntityId,
    relation_kinds: &BTreeSet<GraphRelationKind>,
    incoming: bool,
    cancellation: &dyn GraphCancellation,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
    label_keys_cache: &LabelKeyCache,
    adjacency_ids: &AdjacencyIdIndexCache,
) -> Result<Arc<[GraphRelationId]>, GraphDbError> {
    let key = AdjacencyIndexKey::new(namespace, start, incoming, relation_kinds);
    if let Some(ids) = adjacency_ids.get(&key)? {
        crate::hotpath_observe::record_adjacency_index_hit();
        return Ok(ids);
    }
    crate::hotpath_observe::record_adjacency_index_build();
    let store = database.graph_store();
    let projected =
        relation_projection_cached(Arc::clone(&store), relation_kinds, label_keys_cache)?;
    let Some(node) = optional_node_for_entity(store.as_ref(), namespace, start)? else {
        return adjacency_ids.insert(key, Arc::<[GraphRelationId]>::from(Vec::new()));
    };
    let direction = if incoming {
        Direction::Incoming
    } else {
        Direction::Outgoing
    };
    let mut ids = Vec::new();
    for (_, edge) in projected.edges_from(node, direction) {
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let stored = store.get_edge(edge).ok_or_else(|| GraphDbError::Corrupt {
            message: "outgoing relation references a missing native edge".to_owned(),
        })?;
        let decoded = decode_relation_identity(&stored, namespace)?;
        if !relation_kinds.is_empty() && !relation_kinds.contains(&decoded.kind) {
            return Err(GraphDbError::Corrupt {
                message: "relation kind escaped its projection filter".to_owned(),
            });
        }
        let _ = (&decoded.from, &decoded.to);
        ensure_projection_readable(namespace, &decoded.projection)?;
        ids.push(decoded.identity);
    }
    ids.sort();
    ids.dedup();
    adjacency_ids.insert(key, Arc::<[GraphRelationId]>::from(ids))
}

fn relation_projection_cached(
    store: Arc<dyn GraphStoreSearch>,
    relation_kinds: &BTreeSet<GraphRelationKind>,
    label_keys_cache: &LabelKeyCache,
) -> Result<GraphProjection, GraphDbError> {
    let mut spec = ProjectionSpec::new()
        .with_node_labels(label_keys_cache.keys(store.as_ref(), ENTITY_LABEL)?);
    if !relation_kinds.is_empty() {
        spec = spec.with_edge_types(relation_kinds.iter().map(relation_type_for_kind));
    }
    Ok(GraphProjection::new(store, spec))
}

/// Shared bulk fan-out over one edge direction.
///
/// A start with no projected entity yields an empty batch rather than an
/// error, matching the existing outgoing contract. The `max_relations` budget
/// is charged across the whole batch — not per start — so a caller cannot
/// exceed it by widening `starts`. [`RelationFanoutOverflow::Refuse`] fails
/// with [`GraphDbError::BudgetExhausted`] the moment the next row would
/// exceed the budget (without walking the remaining edges).
/// [`RelationFanoutOverflow::Truncate`] stops and returns the prefix.
#[allow(clippy::too_many_arguments)]
#[hotpath::measure(label = "graph_db.compact.directed_relations")]
fn directed_relations(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    starts: &[GraphEntityId],
    relation_kinds: &BTreeSet<GraphRelationKind>,
    max_relations: usize,
    direction: Direction,
    cancellation: &dyn GraphCancellation,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
    overflow: RelationFanoutOverflow,
) -> Result<Vec<Vec<GraphRelation>>, GraphDbError> {
    check_batch_request(starts, cancellation)?;
    let store = database.graph_store();
    let projected = relation_projection(Arc::clone(&store), relation_kinds);
    let mut admitted = 0_usize;
    let mut results = Vec::with_capacity(starts.len());
    let mut entity_ids = HashMap::new();
    let mut truncated = false;
    for start in starts {
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        if truncated {
            results.push(Vec::new());
            continue;
        }
        let Some(node) = optional_node_for_entity(store.as_ref(), namespace, start)? else {
            results.push(Vec::new());
            continue;
        };
        let mut relations = Vec::new();
        for (_, edge) in projected.edges_from(node, direction) {
            if cancellation.is_cancelled() {
                return Err(GraphDbError::Cancelled);
            }
            if admitted.saturating_add(relations.len()).saturating_add(1) > max_relations {
                match overflow {
                    RelationFanoutOverflow::Refuse => return Err(read_budget(max_relations)),
                    RelationFanoutOverflow::Truncate => {
                        truncated = true;
                        break;
                    }
                }
            }
            relations.push(relation_for_edge(
                &projected,
                edge,
                namespace,
                ensure_projection_readable,
                RelationEndpointCheck::Cached(&mut entity_ids),
            )?);
        }
        relations.sort_by(|left, right| left.identity.cmp(&right.identity));
        relations.dedup_by(|left, right| left.identity == right.identity);
        admitted = admitted
            .checked_add(relations.len())
            .ok_or_else(|| read_budget(max_relations))?;
        results.push(relations);
    }
    Ok(results)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn reachable_entities(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    starts: &[GraphEntityId],
    relation_kinds: &BTreeSet<GraphRelationKind>,
    outgoing_overrides: &BTreeMap<GraphEntityId, BTreeSet<GraphEntityId>>,
    max_visits: usize,
    cancellation: &dyn GraphCancellation,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
) -> Result<Vec<BTreeSet<GraphEntityId>>, GraphDbError> {
    check_batch_request(starts, cancellation)?;
    let store = database.graph_store();
    let projected =
        projection_relation_projection(Arc::clone(&store), namespace, projection, relation_kinds);
    let mut admitted = 0_usize;
    let mut results = Vec::with_capacity(starts.len());
    for start in starts {
        let visited = projected_reachable(
            store.as_ref(),
            &projected,
            namespace,
            projection,
            start,
            outgoing_overrides,
            max_visits,
            &mut admitted,
            cancellation,
            ensure_projection_readable,
        )?;
        results.push(visited);
    }
    Ok(results)
}

fn check_batch_request(
    starts: &[GraphEntityId],
    cancellation: &dyn GraphCancellation,
) -> Result<(), GraphDbError> {
    if cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }
    if starts.len() > MAX_BATCH_TRAVERSAL_STARTS {
        return Err(read_budget(MAX_BATCH_TRAVERSAL_STARTS));
    }
    Ok(())
}

fn validate_request(request: &TraversalRequest) -> Result<(), GraphDbError> {
    if request.cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }
    if request.max_visits == 0 {
        return Err(read_budget(request.max_visits));
    }
    Ok(())
}

fn relation_projection(
    store: Arc<dyn GraphStoreSearch>,
    relation_kinds: &BTreeSet<GraphRelationKind>,
) -> GraphProjection {
    let mut spec = ProjectionSpec::new().with_node_labels(label_keys(store.as_ref(), ENTITY_LABEL));
    if !relation_kinds.is_empty() {
        spec = spec.with_edge_types(relation_kinds.iter().map(relation_type_for_kind));
    }
    GraphProjection::new(store, spec)
}

fn projection_relation_projection(
    store: Arc<dyn GraphStoreSearch>,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    relation_kinds: &BTreeSet<GraphRelationKind>,
) -> GraphProjection {
    let mut spec = ProjectionSpec::new().with_node_labels(label_keys(
        store.as_ref(),
        &entity_projection_label(namespace, projection),
    ));
    if !relation_kinds.is_empty() {
        spec = spec.with_edge_types(relation_kinds.iter().map(relation_type_for_kind));
    }
    GraphProjection::new(store, spec)
}

#[hotpath::measure(label = "graph_db.compact.native_outgoing")]
fn native_outgoing_traversal(
    store: &dyn GraphStore,
    start: NodeId,
    request: &TraversalRequest,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
) -> Result<TraversalResult, GraphDbError> {
    let mut depths = HashMap::from([(start, 0_usize)]);
    let mut entities = HashMap::new();
    let mut entity_ids = HashMap::new();
    let mut relation_ids = HashMap::new();
    let mut via_relations = HashMap::<NodeId, GraphRelationId>::new();
    let mut unfinished_by_depth = HashMap::<usize, usize>::new();
    let mut cutoff_depth = None;
    let mut admitted = 0_usize;

    let stopped = bfs_with_visitor(store, start, |event| {
        if request.cancellation.is_cancelled() {
            return Control::Break(NativeTraversalStop::Error(GraphDbError::Cancelled));
        }
        match event {
            TraversalEvent::Discover(node) => {
                let Some(next_admitted) = admitted.checked_add(1) else {
                    return Control::Break(NativeTraversalStop::Error(read_budget(
                        request.max_visits,
                    )));
                };
                admitted = next_admitted;
                if admitted > request.max_visits {
                    return Control::Break(NativeTraversalStop::Error(read_budget(
                        request.max_visits,
                    )));
                }
                let Some(depth) = depths.get(&node).copied() else {
                    return Control::Break(NativeTraversalStop::Error(GraphDbError::Corrupt {
                        message: "native BFS discovered a node without a depth".to_owned(),
                    }));
                };
                match cached_entity_identity(store, node, &request.namespace, &mut entity_ids) {
                    Ok(identity) => {
                        entities.insert(node, identity);
                        let unfinished = unfinished_by_depth.entry(depth).or_default();
                        let Some(next_unfinished) = unfinished.checked_add(1) else {
                            return Control::Break(NativeTraversalStop::Error(read_budget(
                                request.max_visits,
                            )));
                        };
                        *unfinished = next_unfinished;
                        if entities.len() >= request.max_results && cutoff_depth.is_none() {
                            cutoff_depth = Some(depth);
                            if depth == 0 {
                                return Control::Break(NativeTraversalStop::Complete);
                            }
                        }
                        Control::Continue
                    }
                    Err(error) => Control::Break(NativeTraversalStop::Error(error)),
                }
            }
            TraversalEvent::TreeEdge {
                source,
                target,
                edge,
            } => {
                let Some(source_depth) = depths.get(&source).copied() else {
                    return Control::Break(NativeTraversalStop::Error(GraphDbError::Corrupt {
                        message: "native BFS discovered an edge before its source".to_owned(),
                    }));
                };
                if source_depth >= request.max_depth {
                    return Control::Prune;
                }
                let Some(target_depth) = source_depth.checked_add(1) else {
                    return Control::Break(NativeTraversalStop::Error(read_budget(
                        request.max_depth,
                    )));
                };
                let relation = match cached_relation_identity(
                    store,
                    edge,
                    &request.namespace,
                    ensure_projection_readable,
                    &mut relation_ids,
                ) {
                    Ok(relation) => relation,
                    Err(error) => {
                        return Control::Break(NativeTraversalStop::Error(error));
                    }
                };
                depths.insert(target, target_depth);
                via_relations.insert(target, relation);
                Control::Continue
            }
            TraversalEvent::NonTreeEdge {
                source,
                target,
                edge,
            } => {
                let Some(source_depth) = depths.get(&source).copied() else {
                    return Control::Break(NativeTraversalStop::Error(GraphDbError::Corrupt {
                        message: "native BFS observed an edge from an undiscovered source"
                            .to_owned(),
                    }));
                };
                let Some(target_depth) = depths.get(&target).copied() else {
                    if source_depth >= request.max_depth {
                        return Control::Continue;
                    }
                    return Control::Break(NativeTraversalStop::Error(GraphDbError::Corrupt {
                        message: "native BFS observed an edge to an undiscovered target".to_owned(),
                    }));
                };
                if source_depth.checked_add(1) == Some(target_depth) {
                    let relation = match cached_relation_identity(
                        store,
                        edge,
                        &request.namespace,
                        ensure_projection_readable,
                        &mut relation_ids,
                    ) {
                        Ok(relation) => relation,
                        Err(error) => {
                            return Control::Break(NativeTraversalStop::Error(error));
                        }
                    };
                    via_relations
                        .entry(target)
                        .and_modify(|current| {
                            if relation < *current {
                                *current = relation.clone();
                            }
                        })
                        .or_insert(relation);
                }
                Control::Continue
            }
            TraversalEvent::Finish(node) => {
                let Some(depth) = depths.get(&node).copied() else {
                    return Control::Break(NativeTraversalStop::Error(GraphDbError::Corrupt {
                        message: "native BFS finished a node without a depth".to_owned(),
                    }));
                };
                let Some(unfinished) = unfinished_by_depth.get_mut(&depth) else {
                    return Control::Break(NativeTraversalStop::Error(GraphDbError::Corrupt {
                        message: "native BFS finished an uncounted node".to_owned(),
                    }));
                };
                let Some(remaining) = (*unfinished).checked_sub(1) else {
                    return Control::Break(NativeTraversalStop::Error(GraphDbError::Corrupt {
                        message: "native BFS finished a node twice".to_owned(),
                    }));
                };
                *unfinished = remaining;
                if cutoff_depth == depth.checked_add(1) && remaining == 0 {
                    Control::Break(NativeTraversalStop::Complete)
                } else {
                    Control::Continue
                }
            }
            TraversalEvent::BackEdge { .. } => Control::Continue,
        }
    });
    if let Some(NativeTraversalStop::Error(error)) = stopped {
        return Err(error);
    }
    if request.cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }

    let mut visits = entities
        .into_iter()
        .map(|(node, entity)| {
            let depth = depths
                .get(&node)
                .copied()
                .ok_or_else(|| GraphDbError::Corrupt {
                    message: "native BFS returned a node without a depth".to_owned(),
                })?;
            Ok(TraversalVisit {
                entity,
                depth,
                via_relation: via_relations.remove(&node),
            })
        })
        .collect::<Result<Vec<_>, GraphDbError>>()?;
    visits.sort_by(|left, right| {
        (&left.depth, &left.entity, &left.via_relation).cmp(&(
            &right.depth,
            &right.entity,
            &right.via_relation,
        ))
    });
    visits.truncate(request.max_results);
    Ok(TraversalResult { visits })
}

enum NativeTraversalStop {
    Complete,
    Error(GraphDbError),
}

#[hotpath::measure(label = "graph_db.compact.directional")]
fn directional_traversal(
    store: &dyn GraphStore,
    start: NodeId,
    request: &TraversalRequest,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
) -> Result<TraversalResult, GraphDbError> {
    let mut queue = VecDeque::from([(start, 0_usize, None)]);
    let mut discovered = HashSet::from([start]);
    let mut visits = Vec::new();
    let mut admitted = 0_usize;
    // Identity decoding costs a property fetch plus a validated allocation,
    // and a BFS touches each node once per incident edge (and each edge once
    // per endpoint under `Both`), so memoize decoded identities per run.
    let mut entity_ids: HashMap<NodeId, GraphEntityId> = HashMap::new();
    let mut relation_ids: HashMap<EdgeId, GraphRelationId> = HashMap::new();

    while let Some((node, depth, via_relation)) = queue.pop_front() {
        if request.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        admitted = admitted
            .checked_add(1)
            .ok_or_else(|| read_budget(request.max_visits))?;
        if admitted > request.max_visits {
            return Err(read_budget(request.max_visits));
        }
        visits.push(TraversalVisit {
            entity: cached_entity_identity(store, node, &request.namespace, &mut entity_ids)?,
            depth,
            via_relation,
        });
        if visits.len() >= request.max_results {
            break;
        }
        if depth >= request.max_depth {
            continue;
        }

        let directions: &[Direction] = match request.direction {
            GraphTraversalDirection::Outgoing => &[Direction::Outgoing],
            GraphTraversalDirection::Incoming => &[Direction::Incoming],
            GraphTraversalDirection::Both => &[Direction::Outgoing, Direction::Incoming],
        };
        let mut adjacent = Vec::new();
        for direction in directions {
            for (neighbor, edge) in store.edges_from(node, *direction) {
                if request.cancellation.is_cancelled() {
                    return Err(GraphDbError::Cancelled);
                }
                let relation = cached_relation_identity(
                    store,
                    edge,
                    &request.namespace,
                    ensure_projection_readable,
                    &mut relation_ids,
                )?;
                let entity =
                    cached_entity_identity(store, neighbor, &request.namespace, &mut entity_ids)?;
                adjacent.push((relation, entity, neighbor));
            }
        }
        adjacent.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));
        adjacent.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);
        for (relation, _, target) in adjacent {
            if request.cancellation.is_cancelled() {
                return Err(GraphDbError::Cancelled);
            }
            if discovered.insert(target) {
                let next_depth = depth
                    .checked_add(1)
                    .ok_or_else(|| read_budget(request.max_depth))?;
                queue.push_back((target, next_depth, Some(relation)));
            }
        }
    }
    Ok(TraversalResult { visits })
}

#[allow(clippy::too_many_arguments)]
fn projected_reachable(
    store: &dyn GraphStore,
    projected: &dyn GraphStore,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    start: &GraphEntityId,
    outgoing_overrides: &BTreeMap<GraphEntityId, BTreeSet<GraphEntityId>>,
    max_visits: usize,
    admitted: &mut usize,
    cancellation: &dyn GraphCancellation,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
) -> Result<BTreeSet<GraphEntityId>, GraphDbError> {
    let mut queue = VecDeque::from([start.clone()]);
    let mut visited = BTreeSet::new();
    let mut entity_ids = HashMap::new();
    while let Some(entity) = queue.pop_front() {
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        if !visited.insert(entity.clone()) {
            continue;
        }
        admit_visit(admitted, max_visits)?;
        if let Some(neighbors) = outgoing_overrides.get(&entity) {
            queue.extend(neighbors.iter().cloned());
            continue;
        }
        let Some(node) = optional_node_for_entity(store, namespace, &entity)? else {
            continue;
        };
        if projected.get_node(node).is_none() {
            continue;
        }
        let mut neighbors = Vec::new();
        for (neighbor, edge) in projected.edges_from(node, Direction::Outgoing) {
            if cancellation.is_cancelled() {
                return Err(GraphDbError::Cancelled);
            }
            if !edge_belongs_to_projection(
                projected,
                edge,
                namespace,
                projection,
                ensure_projection_readable,
            )? {
                continue;
            }
            neighbors.push(cached_entity_identity(
                projected,
                neighbor,
                namespace,
                &mut entity_ids,
            )?);
        }
        neighbors.sort();
        neighbors.dedup();
        queue.extend(neighbors);
    }
    Ok(visited)
}

fn edge_belongs_to_projection(
    store: &dyn GraphStore,
    edge: EdgeId,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
) -> Result<bool, GraphDbError> {
    let stored = store.get_edge(edge).ok_or_else(|| GraphDbError::Corrupt {
        message: "reachable traversal references a missing native relation".to_owned(),
    })?;
    let stored_namespace = required_string_property(
        stored.get_property(NAMESPACE_PROPERTY),
        "reachable relation namespace",
    )?;
    let stored_projection = required_string_property(
        stored.get_property(PROJECTION_PROPERTY),
        "reachable relation projection",
    )?;
    let stored_namespace =
        GraphNamespace::new(stored_namespace).map_err(|error| GraphDbError::Corrupt {
            message: format!("reachable relation has an invalid namespace: {error}"),
        })?;
    let stored_projection =
        GraphProjectionId::new(stored_projection).map_err(|error| GraphDbError::Corrupt {
            message: format!("reachable relation has an invalid projection: {error}"),
        })?;
    ensure_projection_readable(&stored_namespace, &stored_projection)?;
    Ok(stored_namespace == *namespace && stored_projection == *projection)
}

fn admit_visit(admitted: &mut usize, max_visits: usize) -> Result<(), GraphDbError> {
    *admitted = admitted
        .checked_add(1)
        .ok_or_else(|| read_budget(max_visits))?;
    if *admitted > max_visits {
        Err(read_budget(max_visits))
    } else {
        Ok(())
    }
}

fn node_for_entity(
    store: &dyn GraphStore,
    namespace: &GraphNamespace,
    identity: &GraphEntityId,
) -> Result<NodeId, GraphDbError> {
    optional_node_for_entity(store, namespace, identity)?
        .ok_or_else(|| GraphDbError::invalid("traversal start entity does not exist"))
}

fn optional_node_for_entity(
    store: &dyn GraphStore,
    namespace: &GraphNamespace,
    identity: &GraphEntityId,
) -> Result<Option<NodeId>, GraphDbError> {
    let mut matches = crate::state::indexed_nodes(
        store,
        ENTITY_KEY_PROPERTY,
        &entity_key_value(namespace, identity),
        ENTITY_LABEL,
    );
    match matches.len() {
        0 => Ok(None),
        1 => {
            let node = matches.pop().ok_or_else(|| GraphDbError::Corrupt {
                message: "entity locator count changed while resolving traversal start".to_owned(),
            })?;
            let stored = entity_identity(store, node, namespace)?;
            if stored != *identity {
                return Err(GraphDbError::Corrupt {
                    message: "entity locator does not match its native identity".to_owned(),
                });
            }
            Ok(Some(node))
        }
        _ => Err(GraphDbError::Corrupt {
            message: "entity locator returned duplicate native nodes".to_owned(),
        }),
    }
}

/// [`entity_identity`] memoized over one traversal: the first decode verifies
/// the stored node, repeat visits within the same read snapshot reuse it.
fn cached_entity_identity(
    store: &dyn GraphStore,
    node: NodeId,
    namespace: &GraphNamespace,
    cache: &mut HashMap<NodeId, GraphEntityId>,
) -> Result<GraphEntityId, GraphDbError> {
    if let Some(identity) = cache.get(&node) {
        return Ok(identity.clone());
    }
    let identity = entity_identity(store, node, namespace)?;
    cache.insert(node, identity.clone());
    Ok(identity)
}

/// [`relation_identity`] memoized over one traversal, mirroring
/// [`cached_entity_identity`].
fn cached_relation_identity(
    store: &dyn GraphStore,
    edge: EdgeId,
    namespace: &GraphNamespace,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
    cache: &mut HashMap<EdgeId, GraphRelationId>,
) -> Result<GraphRelationId, GraphDbError> {
    if let Some(identity) = cache.get(&edge) {
        return Ok(identity.clone());
    }
    let identity = relation_identity(store, edge, namespace, ensure_projection_readable)?;
    cache.insert(edge, identity.clone());
    Ok(identity)
}

fn entity_identity(
    store: &dyn GraphStore,
    node: NodeId,
    namespace: &GraphNamespace,
) -> Result<GraphEntityId, GraphDbError> {
    let stored = store.get_node(node).ok_or_else(|| GraphDbError::Corrupt {
        message: "traversal references a missing native node".to_owned(),
    })?;
    require_namespace(
        stored.get_property(NAMESPACE_PROPERTY),
        namespace,
        "traversal node",
    )?;
    let identity = stored
        .get_property(ENTITY_ID_PROPERTY)
        .and_then(Value::as_str)
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "traversal node has no native entity identity".to_owned(),
        })?;
    GraphEntityId::new(identity).map_err(|error| GraphDbError::Corrupt {
        message: format!("traversal node has an invalid native entity identity: {error}"),
    })
}

fn relation_identity(
    store: &dyn GraphStore,
    edge: EdgeId,
    namespace: &GraphNamespace,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
) -> Result<GraphRelationId, GraphDbError> {
    let stored = store.get_edge(edge).ok_or_else(|| GraphDbError::Corrupt {
        message: "traversal references a missing native relation".to_owned(),
    })?;
    require_namespace(
        stored.get_property(NAMESPACE_PROPERTY),
        namespace,
        "traversal relation",
    )?;
    let projection = GraphProjectionId::new(required_string_property(
        stored.get_property(PROJECTION_PROPERTY),
        "traversal relation projection",
    )?)
    .map_err(|error| GraphDbError::Corrupt {
        message: format!("traversal relation has an invalid projection: {error}"),
    })?;
    ensure_projection_readable(namespace, &projection)?;
    let native_kind = relation_kind_from_type(stored.edge_type.as_str())?;
    let scalar_kind = stored
        .get_property(RELATION_KIND_PROPERTY)
        .and_then(Value::as_str)
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "traversal relation has no native kind".to_owned(),
        })?;
    if native_kind.as_str() != scalar_kind {
        return Err(GraphDbError::Corrupt {
            message: "traversal relation native type and kind disagree".to_owned(),
        });
    }
    let identity = stored
        .get_property(RELATION_ID_PROPERTY)
        .and_then(Value::as_str)
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "traversal relation has no native identity".to_owned(),
        })?;
    GraphRelationId::new(identity).map_err(|error| GraphDbError::Corrupt {
        message: format!("traversal relation has an invalid native identity: {error}"),
    })
}

enum RelationEndpointCheck<'a> {
    Cached(&'a mut HashMap<NodeId, GraphEntityId>),
    Expected {
        from: &'a GraphEntityId,
        to: &'a GraphEntityId,
    },
}

fn relation_for_edge(
    store: &dyn GraphStore,
    edge: EdgeId,
    namespace: &GraphNamespace,
    ensure_projection_readable: &dyn Fn(
        &GraphNamespace,
        &GraphProjectionId,
    ) -> Result<(), GraphDbError>,
    endpoint_check: RelationEndpointCheck<'_>,
) -> Result<GraphRelation, GraphDbError> {
    let stored = store.get_edge(edge).ok_or_else(|| GraphDbError::Corrupt {
        message: "outgoing relation references a missing native edge".to_owned(),
    })?;
    require_namespace(
        stored.get_property(NAMESPACE_PROPERTY),
        namespace,
        "outgoing relation",
    )?;
    let projection = required_string_property(
        stored.get_property(PROJECTION_PROPERTY),
        "outgoing relation projection",
    )?;
    let projection = GraphProjectionId::new(projection).map_err(|error| GraphDbError::Corrupt {
        message: format!("outgoing relation has an invalid projection: {error}"),
    })?;
    ensure_projection_readable(namespace, &projection)?;
    // Extract the identity from the edge record already loaded above; the
    // namespace and projection readability were just verified, so refetching
    // through `relation_identity` would redo the same work per edge.
    let kind = relation_kind_from_type(stored.edge_type.as_str())?;
    let scalar_kind = stored
        .get_property(RELATION_KIND_PROPERTY)
        .and_then(Value::as_str)
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "traversal relation has no native kind".to_owned(),
        })?;
    if kind.as_str() != scalar_kind {
        return Err(GraphDbError::Corrupt {
            message: "traversal relation native type and kind disagree".to_owned(),
        });
    }
    let identity = stored
        .get_property(RELATION_ID_PROPERTY)
        .and_then(Value::as_str)
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "traversal relation has no native identity".to_owned(),
        })?;
    let identity = GraphRelationId::new(identity).map_err(|error| GraphDbError::Corrupt {
        message: format!("traversal relation has an invalid native identity: {error}"),
    })?;
    let from = required_entity_property(
        stored.get_property(RELATION_FROM_PROPERTY),
        "outgoing relation source",
    )?;
    let to = required_entity_property(
        stored.get_property(RELATION_TO_PROPERTY),
        "outgoing relation target",
    )?;
    let endpoints_match = match endpoint_check {
        RelationEndpointCheck::Cached(entity_ids) => {
            cached_entity_identity(store, stored.src, namespace, entity_ids)? == from
                && cached_entity_identity(store, stored.dst, namespace, entity_ids)? == to
        }
        RelationEndpointCheck::Expected {
            from: expected_from,
            to: expected_to,
        } => expected_from == &from && expected_to == &to,
    };
    if !endpoints_match {
        return Err(GraphDbError::Corrupt {
            message: "outgoing relation endpoints disagree with native adjacency".to_owned(),
        });
    }
    let properties = decode_graph_properties(
        stored
            .properties
            .iter()
            .map(|(key, value)| (key.as_str(), value.clone())),
    )?;
    GraphRelation::new(identity, from, to, kind, properties).map_err(|error| {
        GraphDbError::Corrupt {
            message: format!("outgoing relation payload is invalid: {error}"),
        }
    })
}

fn required_entity_property(
    value: Option<&Value>,
    description: &str,
) -> Result<GraphEntityId, GraphDbError> {
    GraphEntityId::new(required_string_property(value, description)?).map_err(|error| {
        GraphDbError::Corrupt {
            message: format!("{description} is invalid: {error}"),
        }
    })
}

fn required_string_property<'a>(
    value: Option<&'a Value>,
    description: &str,
) -> Result<&'a str, GraphDbError> {
    value
        .and_then(Value::as_str)
        .ok_or_else(|| GraphDbError::Corrupt {
            message: format!("{description} is missing or not a string"),
        })
}

fn require_namespace(
    value: Option<&Value>,
    namespace: &GraphNamespace,
    description: &str,
) -> Result<(), GraphDbError> {
    if value.and_then(Value::as_str) == Some(namespace.as_str()) {
        Ok(())
    } else {
        Err(GraphDbError::Corrupt {
            message: format!("{description} belongs to a foreign namespace"),
        })
    }
}
