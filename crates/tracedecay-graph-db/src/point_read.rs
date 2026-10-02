use std::sync::Arc;

use crate::state::{load_entity, load_relation};
use crate::{
    GraphCancellation, GraphDb, GraphDbError, GraphEntity, GraphEntityId, GraphNamespace,
    GraphRelation, GraphRelationId, GraphSnapshot,
};

impl GraphDb {
    #[tracing::instrument(name = "graph_db.read.entity", level = "trace", skip_all)]
    pub fn entity(
        &self,
        namespace: &GraphNamespace,
        identity: &GraphEntityId,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<GraphEntity>, GraphDbError> {
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let guard = self.read_database(cancellation.as_ref())?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let entity = load_entity(database, namespace, identity)?;
        if let Some(stored) = &entity {
            self.ensure_projection_readable(&stored.namespace, &stored.projection)?;
        }
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        crate::observe::record_counts(usize::from(entity.is_some()), 0, 0, 0);
        crate::observe::record_hydration_source(crate::observe::HydrationSource::Live);
        Ok(entity.map(|stored| stored.entity))
    }

    #[tracing::instrument(name = "graph_db.read.relation", level = "trace", skip_all)]
    pub fn relation(
        &self,
        namespace: &GraphNamespace,
        identity: &GraphRelationId,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<GraphRelation>, GraphDbError> {
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        let guard = self.read_database(cancellation.as_ref())?;
        let database = guard.as_ref().ok_or(GraphDbError::Closed)?;
        let relation = load_relation(database, namespace, identity)?;
        if let Some(stored) = &relation {
            self.ensure_projection_readable(namespace, &stored.projection)?;
        }
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        crate::observe::record_counts(0, usize::from(relation.is_some()), 0, 0);
        crate::observe::record_hydration_source(crate::observe::HydrationSource::Live);
        Ok(relation.map(|stored| stored.relation))
    }
}

impl GraphSnapshot {
    pub fn entity(
        &self,
        namespace: &GraphNamespace,
        identity: &GraphEntityId,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<GraphEntity>, GraphDbError> {
        self.database.entity(namespace, identity, cancellation)
    }

    pub fn relation(
        &self,
        namespace: &GraphNamespace,
        identity: &GraphRelationId,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<GraphRelation>, GraphDbError> {
        self.database.relation(namespace, identity, cancellation)
    }
}
