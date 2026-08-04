use std::sync::Arc;

use crate::state::{load_entity, load_relation};
use crate::{
    GraphCancellation, GraphDb, GraphDbError, GraphEntity, GraphEntityId, GraphNamespace,
    GraphRelation, GraphRelationId, GraphSnapshot,
};

impl GraphDb {
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
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        Ok(entity.map(|stored| stored.entity))
    }

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
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
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
