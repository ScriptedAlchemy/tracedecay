use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use tracedecay_graph_db::{
    GraphDbOwnerAttachmentV1, GraphDbOwnerRegistrationV1, GraphDbRegistration,
};
use tracedecay_runtime_core::store_runtime::registry::{
    CanonicalGraphStoreOwnerRetirementTargetV1, StoreRuntimeKey, StoreRuntimeRegistry,
};
use tracedecay_store::{
    RetainedGraphStoreLeaseV1, RetainedGraphStoreOwnerAttachmentV1, StoreIncarnationV1,
    StoreRuntimeBindingV1, StoreShardIdV1, VerifiedStoreLocatorV1,
};

use super::super::{Result, session_registry_error};
use super::{AtomicGraphCancellationV1, GRAPH_OPEN_DEADLINE};

pub(in crate::daemon::store_runtime::session_registry) async fn open_session_relation_owner(
    registry: &StoreRuntimeRegistry,
    graph_registry: &tracedecay_graph_db::GraphDbRegistry,
    lifecycle_cancelled: &Arc<AtomicBool>,
    incarnation: StoreIncarnationV1,
    shard_id: StoreShardIdV1,
) -> Result<(
    GraphDbOwnerAttachmentV1,
    CanonicalGraphStoreOwnerRetirementTargetV1,
)> {
    let key = StoreRuntimeKey::new(shard_id, incarnation);
    let (store_attachment, store_target) =
        registry
            .attach_graph_store_owner(key)
            .await
            .map_err(|failure| {
                session_registry_error(
                    "attach exact session relation graph owner",
                    format!("{failure:?}"),
                )
            })?;
    // The owner attachment issues the exact ordinary operation synchronously.
    // There is no cancellation point after the attachment is installed and
    // before it is moved into GraphDB map ownership.
    let operation = store_attachment
        .issue_operation_lease()
        .map_err(|failure| {
            session_registry_error(
                "issue session relation graph owner operation",
                format!("{failure:?}"),
            )
        })?;
    let registration = registration(lifecycle_cancelled, operation);
    // Owner publication consumes the sole Store attachment. Keep that final
    // transition synchronous: task cancellation cannot otherwise detach the
    // blocking join while the resolver owns the attachment, stranding the
    // map's Ready owner without a retryable graph authority.
    let graph = graph_registry
        .resolve_owner_attachment(GraphDbOwnerRegistrationV1 {
            operation: registration,
            authority_attachment: Box::new(store_attachment),
        })
        .map_err(|error| {
            session_registry_error("open session relation graph owner", error.to_string())
        })?;
    Ok((graph, store_target))
}

pub(in crate::daemon::store_runtime::session_registry) async fn close_retained_for_shutdown(
    graph_registry: &tracedecay_graph_db::GraphDbRegistry,
    binding: StoreRuntimeBindingV1,
    verified_locator: VerifiedStoreLocatorV1,
) -> Result<()> {
    let graph_registry = graph_registry.clone();
    // This close requires an unleased owner: the registry drain must already
    // have dropped the retained map-owner attachments and every owner-issued
    // graph client lease, and the reconciliation workers must already be
    // joined. A lease that survives the drain is a live consumer, so the
    // typed Conflict below is the correct terminal answer — never retry here.
    tokio::task::spawn_blocking(move || {
        graph_registry.close_retained_for_shutdown(&binding, &verified_locator)
    })
    .await
    .map_err(|error| session_registry_error("join graph shutdown close", error.to_string()))?
    .map(|_| ())
    .map_err(|error| session_registry_error("close graph runtime for shutdown", error.to_string()))
}

impl super::RetainedVerifiedGraphRuntimeV1 {
    /// Exact store identity of the retained memory-graph runtime, captured
    /// for the shutdown close after this owner has been drained and dropped.
    pub(in crate::daemon::store_runtime::session_registry) fn graph_store_identity(
        &self,
    ) -> (StoreRuntimeBindingV1, VerifiedStoreLocatorV1) {
        (
            self.graph.binding().clone(),
            self.graph.verified_locator().clone(),
        )
    }
}

fn registration(
    lifecycle_cancelled: &Arc<AtomicBool>,
    authority: Arc<dyn RetainedGraphStoreLeaseV1>,
) -> GraphDbRegistration {
    GraphDbRegistration {
        authority_lease: authority,
        cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
            lifecycle_cancelled,
        ))),
        lifecycle_cancellation: Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
            lifecycle_cancelled,
        ))),
        deadline: Instant::now() + GRAPH_OPEN_DEADLINE,
    }
}
