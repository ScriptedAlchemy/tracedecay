use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use tracedecay_graph_db::{GraphDb, GraphDbRegistration};
use tracedecay_runtime_core::store_runtime::registry::{
    CanonicalGraphStoreLeaseV1, StoreRuntimeKey, StoreRuntimeRegistry,
};
use tracedecay_store::{
    RetainedGraphStoreLeaseV1, StoreIncarnationV1, StoreRuntimeBindingV1, StoreShardIdV1,
    VerifiedStoreLocatorV1,
};

use super::super::{Result, session_registry_error};
use super::{AtomicGraphCancellationV1, GRAPH_OPEN_DEADLINE};

pub(in crate::daemon::store_runtime::session_registry) struct SessionRelationGraphAttachmentV1 {
    graph: Arc<GraphDb>,
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
}

impl SessionRelationGraphAttachmentV1 {
    pub(in crate::daemon::store_runtime::session_registry) fn into_parts(
        self,
    ) -> (Arc<GraphDb>, StoreRuntimeBindingV1, VerifiedStoreLocatorV1) {
        (self.graph, self.binding, self.verified_locator)
    }
}

pub(in crate::daemon::store_runtime::session_registry) async fn open_session_relation(
    registry: &StoreRuntimeRegistry,
    graph_registry: &tracedecay_graph_db::GraphDbRegistry,
    lifecycle_cancelled: &Arc<AtomicBool>,
    incarnation: StoreIncarnationV1,
    shard_id: StoreShardIdV1,
) -> Result<SessionRelationGraphAttachmentV1> {
    let authority = registry
        .retain_graph_store(StoreRuntimeKey::new(shard_id, incarnation))
        .await
        .map_err(|failure| {
            session_registry_error(
                "retain exact session relation graph authority",
                format!("{failure:?}"),
            )
        })?;
    let binding = authority.binding().clone();
    let verified_locator = authority.verified_locator().clone();
    let registration = registration(lifecycle_cancelled, authority);
    let graph_registry = graph_registry.clone();
    let graph = tokio::task::spawn_blocking(move || graph_registry.resolve(registration))
        .await
        .map_err(|error| {
            session_registry_error("join session relation graph open", error.to_string())
        })?
        .map_err(|error| {
            session_registry_error("open session relation graph runtime", error.to_string())
        })?;
    Ok(SessionRelationGraphAttachmentV1 {
        graph,
        binding,
        verified_locator,
    })
}

pub(in crate::daemon::store_runtime::session_registry) async fn close_retained(
    graph_registry: &tracedecay_graph_db::GraphDbRegistry,
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
) -> Result<()> {
    let graph_registry = graph_registry.clone();
    tokio::task::spawn_blocking(move || graph_registry.close_retained(&binding, &verified_locator))
        .await
        .map_err(|error| session_registry_error("join graph close", error.to_string()))?
        .map(|_| ())
        .map_err(|error| session_registry_error("close graph runtime", error.to_string()))
}

fn registration(
    lifecycle_cancelled: &Arc<AtomicBool>,
    authority: Arc<CanonicalGraphStoreLeaseV1>,
) -> GraphDbRegistration {
    let authority_lease: Arc<dyn RetainedGraphStoreLeaseV1> = authority;
    GraphDbRegistration {
        authority_lease,
        cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
            lifecycle_cancelled,
        ))),
        lifecycle_cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
            lifecycle_cancelled,
        ))),
        deadline: Instant::now() + GRAPH_OPEN_DEADLINE,
    }
}
