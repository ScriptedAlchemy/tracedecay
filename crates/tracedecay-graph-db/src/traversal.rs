use std::collections::{BTreeMap, BTreeSet, HashSet, VecDeque};
use std::fmt;
use std::sync::Arc;

use grafeo_common::types::NodeId;
use grafeo_core::graph::Direction;
use grafeo_engine::GrafeoDB;

use crate::state::{StateCache, StoredEntity, stable_key};
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
    state: &StateCache,
    request: TraversalRequest,
) -> Result<TraversalResult, GraphDbError> {
    if request.cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }
    if request.max_visits == 0 {
        return Err(GraphDbError::BudgetExhausted);
    }
    if request.max_results == 0 {
        return Ok(TraversalResult { visits: Vec::new() });
    }
    let Some((start_node, _)) = state
        .entities
        .get(&stable_key(&request.namespace, request.start.as_str()))
    else {
        return Err(GraphDbError::invalid(
            "traversal start entity does not exist",
        ));
    };

    let store = database.graph_store();
    let mut queue = VecDeque::from([(*start_node, 0_usize, None)]);
    let mut discovered = HashSet::from([*start_node]);
    let mut visits = Vec::new();
    let mut admitted = 0_usize;

    while let Some((node, depth, via_relation)) = queue.pop_front() {
        if request.cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        admitted = admitted.saturating_add(1);
        if admitted > request.max_visits {
            return Err(GraphDbError::BudgetExhausted);
        }
        let stored = entity_for_node(state, node, &request.namespace)?;
        visits.push(TraversalVisit {
            entity: stored.entity.identity.clone(),
            depth,
            via_relation,
        });
        if visits.len() >= request.max_results {
            break;
        }
        if depth >= request.max_depth {
            continue;
        }

        let mut adjacent = Vec::new();
        let directions: &[(Direction, bool)] = match request.direction {
            GraphTraversalDirection::Outgoing => &[(Direction::Outgoing, true)],
            GraphTraversalDirection::Incoming => &[(Direction::Incoming, false)],
            GraphTraversalDirection::Both => {
                &[(Direction::Outgoing, true), (Direction::Incoming, false)]
            }
        };
        for (direction, outgoing) in directions {
            for (neighbor, edge_id) in store.edges_from(node, *direction) {
                if request.cancellation.is_cancelled() {
                    return Err(GraphDbError::Cancelled);
                }
                let relation =
                    state
                        .relation_by_edge(edge_id)
                        .ok_or_else(|| GraphDbError::Corrupt {
                            message: "Grafeo returned an uncached traversal edge".to_owned(),
                        })?;
                if relation.namespace != request.namespace {
                    continue;
                }
                if !request.relation_kinds.is_empty()
                    && !request.relation_kinds.contains(&relation.relation.kind)
                {
                    continue;
                }
                let neighbor = if *outgoing {
                    neighbor
                } else {
                    state
                        .entities
                        .get(&stable_key(
                            &request.namespace,
                            relation.relation.from.as_str(),
                        ))
                        .map(|(node, _)| *node)
                        .ok_or_else(|| GraphDbError::Corrupt {
                            message: "incoming relation source is missing".to_owned(),
                        })?
                };
                let neighbor_entity = entity_for_node(state, neighbor, &request.namespace)?;
                adjacent.push((
                    relation.relation.identity.clone(),
                    neighbor_entity.entity.identity.clone(),
                    neighbor,
                ));
            }
        }
        adjacent.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));
        adjacent.dedup_by(|left, right| left.0 == right.0 && left.1 == right.1);
        for (relation, _, target) in adjacent {
            if request.cancellation.is_cancelled() {
                return Err(GraphDbError::Cancelled);
            }
            if discovered.insert(target) {
                queue.push_back((target, depth + 1, Some(relation)));
            }
        }
    }
    Ok(TraversalResult { visits })
}

pub(crate) fn outgoing_relation_ids(
    database: &GrafeoDB,
    state: &StateCache,
    namespace: &GraphNamespace,
    starts: &[GraphEntityId],
    relation_kinds: &BTreeSet<GraphRelationKind>,
    max_relations: usize,
    cancellation: &dyn GraphCancellation,
) -> Result<Vec<Vec<GraphRelationId>>, GraphDbError> {
    Ok(outgoing_relations(
        database,
        state,
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
    state: &StateCache,
    namespace: &GraphNamespace,
    starts: &[GraphEntityId],
    relation_kinds: &BTreeSet<GraphRelationKind>,
    max_relations: usize,
    cancellation: &dyn GraphCancellation,
) -> Result<Vec<Vec<GraphRelation>>, GraphDbError> {
    if cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }
    if starts.len() > MAX_BATCH_TRAVERSAL_STARTS {
        return Err(GraphDbError::BudgetExhausted);
    }
    let store = database.graph_store();
    let mut admitted = 0_usize;
    let mut results = Vec::with_capacity(starts.len());
    for start in starts {
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let Some((node, stored)) = state.entities.get(&stable_key(namespace, start.as_str()))
        else {
            results.push(Vec::new());
            continue;
        };
        if stored.namespace != *namespace || stored.entity.identity != *start {
            return Err(GraphDbError::Corrupt {
                message: "outgoing relation start index does not match its payload".to_owned(),
            });
        }
        let mut relations = Vec::new();
        for (_, edge_id) in store.edges_from(*node, Direction::Outgoing) {
            if cancellation.is_cancelled() {
                return Err(GraphDbError::Cancelled);
            }
            let relation =
                state
                    .relation_by_edge(edge_id)
                    .ok_or_else(|| GraphDbError::Corrupt {
                        message: "Grafeo returned an uncached outgoing relation".to_owned(),
                    })?;
            if relation.namespace != *namespace
                || (!relation_kinds.is_empty() && !relation_kinds.contains(&relation.relation.kind))
            {
                continue;
            }
            if relation.relation.from != *start {
                return Err(GraphDbError::Corrupt {
                    message: "outgoing relation index does not match its payload".to_owned(),
                });
            }
            relations.push(relation.relation.clone());
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

pub(crate) fn reachable_entities(
    database: &GrafeoDB,
    state: &StateCache,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    starts: &[GraphEntityId],
    relation_kinds: &BTreeSet<GraphRelationKind>,
    outgoing_overrides: &BTreeMap<GraphEntityId, BTreeSet<GraphEntityId>>,
    max_visits: usize,
    cancellation: &dyn GraphCancellation,
) -> Result<Vec<BTreeSet<GraphEntityId>>, GraphDbError> {
    if cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }
    if starts.len() > MAX_BATCH_TRAVERSAL_STARTS {
        return Err(GraphDbError::BudgetExhausted);
    }
    let store = database.graph_store();
    let mut admitted = 0_usize;
    let mut results = Vec::with_capacity(starts.len());
    for start in starts {
        let mut queue = VecDeque::from([start.clone()]);
        let mut visited = BTreeSet::new();
        while let Some(entity) = queue.pop_front() {
            if cancellation.is_cancelled() {
                return Err(GraphDbError::Cancelled);
            }
            if !visited.insert(entity.clone()) {
                continue;
            }
            admitted = admitted
                .checked_add(1)
                .ok_or(GraphDbError::BudgetExhausted)?;
            if admitted > max_visits {
                return Err(GraphDbError::BudgetExhausted);
            }
            if let Some(neighbors) = outgoing_overrides.get(&entity) {
                queue.extend(neighbors.iter().cloned());
                continue;
            }
            let Some((node, stored)) = state.entities.get(&stable_key(namespace, entity.as_str()))
            else {
                continue;
            };
            if stored.namespace != *namespace || stored.entity.identity != entity {
                return Err(GraphDbError::Corrupt {
                    message: "reachable entity index does not match its payload".to_owned(),
                });
            }
            if stored.projection != *projection {
                continue;
            }
            let mut neighbors = Vec::new();
            for (neighbor, edge_id) in store.edges_from(*node, Direction::Outgoing) {
                if cancellation.is_cancelled() {
                    return Err(GraphDbError::Cancelled);
                }
                let relation =
                    state
                        .relation_by_edge(edge_id)
                        .ok_or_else(|| GraphDbError::Corrupt {
                            message: "Grafeo returned an uncached reachable relation".to_owned(),
                        })?;
                if relation.namespace != *namespace
                    || relation.projection != *projection
                    || (!relation_kinds.is_empty()
                        && !relation_kinds.contains(&relation.relation.kind))
                {
                    continue;
                }
                let neighbor = entity_for_node(state, neighbor, namespace)?
                    .entity
                    .identity
                    .clone();
                neighbors.push(neighbor);
            }
            neighbors.sort();
            neighbors.dedup();
            queue.extend(neighbors);
        }
        results.push(visited);
    }
    Ok(results)
}

fn entity_for_node<'a>(
    state: &'a StateCache,
    node: NodeId,
    namespace: &GraphNamespace,
) -> Result<&'a StoredEntity, GraphDbError> {
    state
        .entity_by_node(node)
        .filter(|entity| entity.namespace == *namespace)
        .ok_or_else(|| GraphDbError::Corrupt {
            message: "relation targets a missing or foreign-namespace entity".to_owned(),
        })
}
