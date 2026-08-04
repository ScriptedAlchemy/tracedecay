use std::sync::Arc;

use crate::state::{StateCache, stable_key};
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
        let state = self.point_read_state(cancellation.as_ref())?;
        read_entity(&state, namespace, identity)
    }

    pub fn relation(
        &self,
        namespace: &GraphNamespace,
        identity: &GraphRelationId,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<GraphRelation>, GraphDbError> {
        let state = self.point_read_state(cancellation.as_ref())?;
        read_relation(&state, namespace, identity)
    }
}

impl GraphSnapshot {
    pub fn entity(
        &self,
        namespace: &GraphNamespace,
        identity: &GraphEntityId,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<GraphEntity>, GraphDbError> {
        if cancellation.is_cancelled() {
            return Err(GraphDbError::Cancelled);
        }
        read_entity(&self.state, namespace, identity)
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
        read_relation(&self.state, namespace, identity)
    }
}

fn read_entity(
    state: &StateCache,
    namespace: &GraphNamespace,
    identity: &GraphEntityId,
) -> Result<Option<GraphEntity>, GraphDbError> {
    let Some((_, stored)) = state
        .entities
        .get(&stable_key(namespace, identity.as_str()))
    else {
        return Ok(None);
    };
    if stored.namespace != *namespace || stored.entity.identity != *identity {
        return Err(GraphDbError::Corrupt {
            message: "entity point-read index does not match its payload".to_owned(),
        });
    }
    Ok(Some(stored.entity.clone()))
}

fn read_relation(
    state: &StateCache,
    namespace: &GraphNamespace,
    identity: &GraphRelationId,
) -> Result<Option<GraphRelation>, GraphDbError> {
    let Some((_, stored)) = state
        .relations
        .get(&stable_key(namespace, identity.as_str()))
    else {
        return Ok(None);
    };
    if stored.namespace != *namespace || stored.relation.identity != *identity {
        return Err(GraphDbError::Corrupt {
            message: "relation point-read index does not match its payload".to_owned(),
        });
    }
    Ok(Some(stored.relation.clone()))
}
