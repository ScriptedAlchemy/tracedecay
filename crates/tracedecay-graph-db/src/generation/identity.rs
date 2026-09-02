use serde::{Deserialize, Serialize};

use crate::{
    GraphEntityId, GraphGenerationId, GraphIdempotencyKey, GraphNamespace, GraphProjectionId,
    GraphRelationId,
};

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GraphProjectionIdentity {
    pub namespace: GraphNamespace,
    pub projection: GraphProjectionId,
}

impl GraphProjectionIdentity {
    #[must_use]
    pub fn new(namespace: GraphNamespace, projection: GraphProjectionId) -> Self {
        Self {
            namespace,
            projection,
        }
    }
}

impl std::fmt::Display for GraphProjectionIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.namespace, self.projection)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GraphGenerationDependency {
    pub projection: GraphProjectionIdentity,
    pub generation: GraphGenerationId,
    pub idempotency_key: GraphIdempotencyKey,
}

impl GraphGenerationDependency {
    #[must_use]
    pub fn new(
        projection: GraphProjectionIdentity,
        generation: GraphGenerationId,
        idempotency_key: GraphIdempotencyKey,
    ) -> Self {
        Self {
            projection,
            generation,
            idempotency_key,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GraphEntityRef {
    pub projection: GraphProjectionIdentity,
    pub identity: GraphEntityId,
}

impl GraphEntityRef {
    #[must_use]
    pub fn new(projection: GraphProjectionIdentity, identity: GraphEntityId) -> Self {
        Self {
            projection,
            identity,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GraphRelationRef {
    pub projection: GraphProjectionIdentity,
    pub identity: GraphRelationId,
}

impl GraphRelationRef {
    #[must_use]
    pub fn new(projection: GraphProjectionIdentity, identity: GraphRelationId) -> Self {
        Self {
            projection,
            identity,
        }
    }
}
