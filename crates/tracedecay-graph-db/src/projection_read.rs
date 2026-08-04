use std::sync::Arc;

use grafeo_engine::GrafeoDB;

use crate::schema::entity_projection_domain_label;
use crate::state::load_entity_by_node;
use crate::{
    GraphCancellation, GraphDb, GraphDbError, GraphEntity, GraphLabel, GraphNamespace,
    GraphProjectionId,
};

const MAX_LABEL_PAGE_ENTITIES: usize = 100_000;

#[derive(Clone, Debug, PartialEq)]
pub struct GraphProjectionLabelPage {
    pub entities: Vec<GraphEntity>,
    pub total_entities: u64,
}

impl GraphDb {
    /// Reads one bounded exact-label page from a projection-scoped native
    /// label index. The total counts only entities carrying this exact label;
    /// reference-only nodes in the same projection are excluded.
    pub fn projection_entities_by_label(
        &self,
        namespace: &GraphNamespace,
        projection: &GraphProjectionId,
        label: &GraphLabel,
        limit: usize,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<GraphProjectionLabelPage, GraphDbError> {
        if limit == 0 || limit > MAX_LABEL_PAGE_ENTITIES {
            return Err(GraphDbError::BudgetExhausted);
        }
        let guard = self.read_database(cancellation.as_ref())?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        projection_entities_by_label(
            database,
            namespace,
            projection,
            label,
            limit,
            cancellation.as_ref(),
        )
    }
}

fn projection_entities_by_label(
    database: &GrafeoDB,
    namespace: &GraphNamespace,
    projection: &GraphProjectionId,
    label: &GraphLabel,
    limit: usize,
    cancellation: &dyn GraphCancellation,
) -> Result<GraphProjectionLabelPage, GraphDbError> {
    if cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }
    let mut nodes = database
        .graph_store()
        .nodes_by_label(&entity_projection_domain_label(
            namespace, projection, label,
        ));
    nodes.sort_unstable();
    let total_entities = u64::try_from(nodes.len()).map_err(|_| GraphDbError::Corrupt {
        message: "projection label cardinality exceeds u64".to_owned(),
    })?;
    let mut entities = Vec::with_capacity(nodes.len().min(limit));
    for node in nodes.into_iter().take(limit) {
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let stored = load_entity_by_node(database, node)?;
        if stored.namespace != *namespace
            || stored.projection != *projection
            || !stored.entity.labels.contains(label)
        {
            return Err(GraphDbError::Corrupt {
                message: "projection label index does not match entity ownership".to_owned(),
            });
        }
        entities.push(stored.entity);
    }
    entities.sort_by(|left, right| left.identity.cmp(&right.identity));
    if cancellation.is_cancelled() {
        return Err(GraphDbError::Cancelled);
    }
    Ok(GraphProjectionLabelPage {
        entities,
        total_entities,
    })
}
