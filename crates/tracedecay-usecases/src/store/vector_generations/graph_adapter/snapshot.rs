use std::sync::Arc;

use tracedecay_graph_db::{
    GraphCancellation, GraphDbError, GraphEntity, GraphEntityId, GraphEntityRef,
    GraphGenerationRelation, GraphNamespace, GraphProjectionId, GraphProjectionPage,
    GraphProjectionReadRequest, GraphProjectionTelemetry, GraphProjectionTelemetryRequest,
    GraphRelation, GraphRelationId, GraphRelationRef, TraversalRequest, TraversalResult,
    TraversalVisit, VectorSearchRequest, VectorSearchResult, VerifiedGraphSnapshot,
};

/// Exact verified read authority for one semantic-vector projection generation.
#[derive(Clone)]
pub(super) struct SemanticVectorVerifiedRead {
    inner: VerifiedGraphSnapshot,
}

pub(super) type SemanticVectorVerifiedReadV1 = SemanticVectorVerifiedRead;

impl SemanticVectorVerifiedRead {
    pub(super) fn new(inner: VerifiedGraphSnapshot) -> Self {
        Self { inner }
    }

    pub(super) fn verified(&self) -> &VerifiedGraphSnapshot {
        &self.inner
    }

    pub(super) fn projection(&self) -> &tracedecay_graph_db::GraphProjectionIdentity {
        self.inner.projection()
    }

    pub(super) fn entity(
        &self,
        namespace: &GraphNamespace,
        identity: &GraphEntityId,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<GraphEntity>, GraphDbError> {
        self.require_projection(namespace, &self.inner.projection().projection)?;
        self.inner.entity(
            &GraphEntityRef::new(self.inner.projection().clone(), identity.clone()),
            cancellation,
        )
    }

    pub(super) fn relation(
        &self,
        namespace: &GraphNamespace,
        identity: &GraphRelationId,
        cancellation: Arc<dyn GraphCancellation>,
    ) -> Result<Option<GraphRelation>, GraphDbError> {
        self.require_projection(namespace, &self.inner.projection().projection)?;
        self.inner
            .relation(
                &GraphRelationRef::new(self.inner.projection().clone(), identity.clone()),
                cancellation,
            )
            .and_then(|relation| relation.map(storage_relation).transpose())
    }

    pub(super) fn read_projection(
        &self,
        request: GraphProjectionReadRequest,
    ) -> Result<GraphProjectionPage, GraphDbError> {
        self.require_projection(&request.namespace, &request.projection)?;
        self.inner.read_projection(request)
    }

    pub(super) fn projection_telemetry(
        &self,
        request: GraphProjectionTelemetryRequest,
    ) -> Result<Option<GraphProjectionTelemetry>, GraphDbError> {
        self.require_projection(&request.namespace, &request.projection)?;
        self.inner.projection_telemetry(request)
    }

    pub(super) fn vector_search(
        &self,
        request: VectorSearchRequest,
    ) -> Result<VectorSearchResult, GraphDbError> {
        self.require_projection(&request.namespace, &request.projection)?;
        self.inner.vector_search(request)
    }

    pub(super) fn traverse(
        &self,
        request: TraversalRequest,
    ) -> Result<TraversalResult, GraphDbError> {
        self.require_projection(&request.namespace, &self.inner.projection().projection)?;
        self.inner.traverse(request).map(|result| TraversalResult {
            visits: result
                .visits
                .into_iter()
                .map(|visit| TraversalVisit {
                    entity: visit.entity.identity,
                    depth: visit.depth,
                    via_relation: visit.via_relation.map(|relation| relation.identity),
                })
                .collect(),
        })
    }

    fn require_projection(
        &self,
        namespace: &GraphNamespace,
        projection: &GraphProjectionId,
    ) -> Result<(), GraphDbError> {
        if namespace != &self.inner.projection().namespace
            || projection != &self.inner.projection().projection
        {
            return Err(GraphDbError::Conflict);
        }
        Ok(())
    }
}

fn storage_relation(relation: GraphGenerationRelation) -> Result<GraphRelation, GraphDbError> {
    GraphRelation::new(
        relation.identity,
        relation.from.identity,
        relation.to.identity,
        relation.kind,
        relation.properties,
    )
}
