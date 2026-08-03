use std::collections::{BTreeSet, HashSet, VecDeque};
use std::fmt;
use std::sync::Arc;

use grafeo_common::types::NodeId;
use grafeo_core::graph::Direction;
use grafeo_engine::GrafeoDB;

use crate::state::{StateCache, StoredEntity, stable_key};
use crate::{
    GraphCancellation, GraphDbError, GraphEntityId, GraphNamespace, GraphRelationId,
    GraphRelationKind,
};

#[derive(Clone)]
pub struct TraversalRequest {
    pub namespace: GraphNamespace,
    pub start: GraphEntityId,
    pub relation_kinds: BTreeSet<GraphRelationKind>,
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

        let mut outgoing = Vec::new();
        for (target, edge_id) in store.edges_from(node, Direction::Outgoing) {
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
            let target_entity = entity_for_node(state, target, &request.namespace)?;
            outgoing.push((
                relation.relation.identity.clone(),
                target_entity.entity.identity.clone(),
                target,
            ));
        }
        outgoing.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));
        for (relation, _, target) in outgoing {
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
