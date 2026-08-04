use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::fmt;
use std::sync::Arc;

use grafeo_adapters::plugins::algorithms::{Control, TraversalEvent, bfs_with_visitor};
use grafeo_common::types::{EdgeId, NodeId, Value};
use grafeo_core::graph::{
    Direction, GraphProjection, GraphStore, GraphStoreSearch, ProjectionSpec,
};
use grafeo_engine::GrafeoDB;

use crate::schema::{
    ENTITY_ID_PROPERTY, ENTITY_LABEL, NAMESPACE_PROPERTY, PROJECTION_PROPERTY,
    RELATION_FROM_PROPERTY, RELATION_ID_PROPERTY, RELATION_KIND_PROPERTY, RELATION_TO_PROPERTY,
    decode_graph_properties, entity_key_label, entity_projection_label, relation_kind_from_type,
    relation_type_for_kind,
};
use crate::{
    GraphCancellation, GraphDbError, GraphEntityId, GraphNamespace, GraphProjectionId,
    GraphRelation, GraphRelationId, GraphRelationKind,
};

const MAX_BATCH_TRAVERSAL_STARTS: usize = 100_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GraphTraversalDirection {
    Outgoing,
    Incoming,
    Both,
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

pub(crate) fn traverse(
    database: &GrafeoDB,
    request: TraversalRequest,
) -> Result<TraversalResult, GraphDbError> {
    validate_request(&request)?;
    if request.max_results == 0 {
        return Ok(TraversalResult { visits: Vec::new() });
    }

    let store = database.graph_store();
    let start = node_for_entity(store.as_ref(), &request.namespace, &request.start)?;
    let projected = relation_projection(store, &request.relation_kinds);
    match request.direction {
        GraphTraversalDirection::Outgoing => native_outgoing_traversal(&projected, start, &request),
        GraphTraversalDirection::Incoming | GraphTraversalDirection::Both => {
            directional_traversal(&projected, start, &request)
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
) -> Result<Vec<Vec<GraphRelationId>>, GraphDbError> {
    Ok(outgoing_relations(
        database,
        namespace,
        starts,
        relation_kinds,
        max_relations,
        cancellation,
    )?
    .into_iter()
    .map(|relations| {
        relations
            .into_iter()
            .map(|relation| relation.identity)
            .collect()
    })
    .collect())
}

pub(crate) fn outgoing_relations(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    starts: &[GraphEntityId],
    relation_kinds: &BTreeSet<GraphRelationKind>,
    max_relations: usize,
    cancellation: &dyn GraphCancellation,
) -> Result<Vec<Vec<GraphRelation>>, GraphDbError> {
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
        let mut relations = Vec::new();
        for (_, edge) in projected.edges_from(node, Direction::Outgoing) {
            if cancellation.is_cancelled() {
                return Err(GraphDbError::Cancelled);
            }
            relations.push(relation_for_edge(&projected, edge, namespace)?);
        }
        relations.sort_by(|left, right| left.identity.cmp(&right.identity));
        relations.dedup_by(|left, right| left.identity == right.identity);
        admitted = admitted
            .checked_add(relations.len())
            .ok_or(GraphDbError::BudgetExhausted)?;
        if admitted > max_relations {
            return Err(GraphDbError::BudgetExhausted);
        }
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
        return Err(GraphDbError::BudgetExhausted);
    }
    Ok(())
}

fn validate_request(request: &TraversalRequest) -> Result<(), GraphDbError> {
    if request.cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }
    if request.max_visits == 0 {
        return Err(GraphDbError::BudgetExhausted);
    }
    Ok(())
}

fn relation_projection(
    store: Arc<dyn GraphStoreSearch>,
    relation_kinds: &BTreeSet<GraphRelationKind>,
) -> GraphProjection {
    let mut spec = ProjectionSpec::new().with_node_labels([ENTITY_LABEL]);
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
    let mut spec =
        ProjectionSpec::new().with_node_labels([entity_projection_label(namespace, projection)]);
    if !relation_kinds.is_empty() {
        spec = spec.with_edge_types(relation_kinds.iter().map(relation_type_for_kind));
    }
    GraphProjection::new(store, spec)
}

fn native_outgoing_traversal(
    store: &dyn GraphStore,
    start: NodeId,
    request: &TraversalRequest,
) -> Result<TraversalResult, GraphDbError> {
    let mut depths = HashMap::from([(start, 0_usize)]);
    let mut entities = HashMap::new();
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
                    return Control::Break(NativeTraversalStop::Error(
                        GraphDbError::BudgetExhausted,
                    ));
                };
                admitted = next_admitted;
                if admitted > request.max_visits {
                    return Control::Break(NativeTraversalStop::Error(
                        GraphDbError::BudgetExhausted,
                    ));
                }
                let Some(depth) = depths.get(&node).copied() else {
                    return Control::Break(NativeTraversalStop::Error(GraphDbError::Corrupt {
                        message: "native BFS discovered a node without a depth".to_owned(),
                    }));
                };
                match entity_identity(store, node, &request.namespace) {
                    Ok(identity) => {
                        entities.insert(node, identity);
                        let unfinished = unfinished_by_depth.entry(depth).or_default();
                        let Some(next_unfinished) = unfinished.checked_add(1) else {
                            return Control::Break(NativeTraversalStop::Error(
                                GraphDbError::BudgetExhausted,
                            ));
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
                    return Control::Break(NativeTraversalStop::Error(
                        GraphDbError::BudgetExhausted,
                    ));
                };
                let relation = match relation_identity(store, edge, &request.namespace) {
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
                    let relation = match relation_identity(store, edge, &request.namespace) {
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

fn directional_traversal(
    store: &dyn GraphStore,
    start: NodeId,
    request: &TraversalRequest,
) -> Result<TraversalResult, GraphDbError> {
    let mut queue = VecDeque::from([(start, 0_usize, None)]);
    let mut discovered = HashSet::from([start]);
    let mut visits = Vec::new();
    let mut admitted = 0_usize;

    while let Some((node, depth, via_relation)) = queue.pop_front() {
        if request.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        admitted = admitted
            .checked_add(1)
            .ok_or(GraphDbError::BudgetExhausted)?;
        if admitted > request.max_visits {
            return Err(GraphDbError::BudgetExhausted);
        }
        visits.push(TraversalVisit {
            entity: entity_identity(store, node, &request.namespace)?,
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
                let relation = relation_identity(store, edge, &request.namespace)?;
                let entity = entity_identity(store, neighbor, &request.namespace)?;
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
                let next_depth = depth.checked_add(1).ok_or(GraphDbError::BudgetExhausted)?;
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
) -> Result<BTreeSet<GraphEntityId>, GraphDbError> {
    let mut queue = VecDeque::from([start.clone()]);
    let mut visited = BTreeSet::new();
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
            if !edge_belongs_to_projection(projected, edge, namespace, projection)? {
                continue;
            }
            neighbors.push(entity_identity(projected, neighbor, namespace)?);
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
    Ok(stored_namespace == namespace.as_str() && stored_projection == projection.as_str())
}

fn admit_visit(admitted: &mut usize, max_visits: usize) -> Result<(), GraphDbError> {
    *admitted = admitted
        .checked_add(1)
        .ok_or(GraphDbError::BudgetExhausted)?;
    if *admitted > max_visits {
        Err(GraphDbError::BudgetExhausted)
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
    let mut matches = store.nodes_by_label(&entity_key_label(namespace, identity));
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
) -> Result<GraphRelationId, GraphDbError> {
    let stored = store.get_edge(edge).ok_or_else(|| GraphDbError::Corrupt {
        message: "traversal references a missing native relation".to_owned(),
    })?;
    require_namespace(
        stored.get_property(NAMESPACE_PROPERTY),
        namespace,
        "traversal relation",
    )?;
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

fn relation_for_edge(
    store: &dyn GraphStore,
    edge: EdgeId,
    namespace: &GraphNamespace,
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
    GraphProjectionId::new(projection).map_err(|error| GraphDbError::Corrupt {
        message: format!("outgoing relation has an invalid projection: {error}"),
    })?;
    let identity = relation_identity(store, edge, namespace)?;
    let from = required_entity_property(
        stored.get_property(RELATION_FROM_PROPERTY),
        "outgoing relation source",
    )?;
    let to = required_entity_property(
        stored.get_property(RELATION_TO_PROPERTY),
        "outgoing relation target",
    )?;
    if entity_identity(store, stored.src, namespace)? != from
        || entity_identity(store, stored.dst, namespace)? != to
    {
        return Err(GraphDbError::Corrupt {
            message: "outgoing relation endpoints disagree with native adjacency".to_owned(),
        });
    }
    let kind = relation_kind_from_type(stored.edge_type.as_str())?;
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
