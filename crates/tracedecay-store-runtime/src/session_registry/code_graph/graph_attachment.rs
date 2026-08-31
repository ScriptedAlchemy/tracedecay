use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use tracedecay_graph_db::{
    GraphCancellation, GraphDbOwnerAttachmentV1, GraphDbOwnerRegistrationV1, GraphDbRegistration,
};
use tracedecay_runtime_core::store_runtime::registry::{
    CanonicalGraphStoreOwnerRetirementTargetV1, StoreRuntimeKey, StoreRuntimeRegistry,
};
use tracedecay_sessions::observation::ObservationCancellation;
use tracedecay_store::{
    RetainedGraphStoreLeaseV1, RetainedGraphStoreOwnerAttachmentV1, StoreIncarnationV1,
    StoreRuntimeBindingV1, StoreShardIdV1, VerifiedStoreLocatorV1,
};

use super::super::{Result, session_registry_error};
use super::{AtomicGraphCancellationV1, GRAPH_OPEN_DEADLINE};

pub async fn open_session_relation_owner(
    registry: &StoreRuntimeRegistry,
    graph_registry: &tracedecay_graph_db::GraphDbRegistry,
    lifecycle_cancelled: &Arc<AtomicBool>,
    incarnation: StoreIncarnationV1,
    shard_id: StoreShardIdV1,
) -> Result<(
    GraphDbOwnerAttachmentV1,
    CanonicalGraphStoreOwnerRetirementTargetV1,
)> {
    open_session_relation_owner_with_cancellation(
        registry,
        graph_registry,
        incarnation,
        shard_id,
        Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
            lifecycle_cancelled,
        ))),
        Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
            lifecycle_cancelled,
        ))),
    )
    .await
}

pub async fn open_session_relation_owner_for_task(
    registry: &StoreRuntimeRegistry,
    graph_registry: &tracedecay_graph_db::GraphDbRegistry,
    lifecycle_cancelled: &Arc<AtomicBool>,
    cancellation: ObservationCancellation,
    incarnation: StoreIncarnationV1,
    shard_id: StoreShardIdV1,
) -> Result<(
    GraphDbOwnerAttachmentV1,
    CanonicalGraphStoreOwnerRetirementTargetV1,
)> {
    open_session_relation_owner_with_cancellation(
        registry,
        graph_registry,
        incarnation,
        shard_id,
        Arc::new(GraphOpenTaskCancellationV1 {
            lifecycle: Arc::clone(lifecycle_cancelled),
            operation: cancellation,
        }),
        Arc::new(AtomicGraphCancellationV1::new(Arc::clone(
            lifecycle_cancelled,
        ))),
    )
    .await
}

async fn open_session_relation_owner_with_cancellation(
    registry: &StoreRuntimeRegistry,
    graph_registry: &tracedecay_graph_db::GraphDbRegistry,
    incarnation: StoreIncarnationV1,
    shard_id: StoreShardIdV1,
    cancellation: Arc<dyn GraphCancellation>,
    lifecycle_cancellation: Arc<dyn GraphCancellation>,
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
    let registration = registration(cancellation, lifecycle_cancellation, operation);
    let graph_registry = graph_registry.clone();
    // Grafeo SingleFile restore is blocking CPU and allocation work. Retain
    // the join in the daemon-owned task while moving the resolver off Tokio's
    // cooperative workers; cancellation remains visible inside the native
    // load through the exact registration authority.
    let graph = tokio::task::spawn_blocking(move || {
        hotpath::measure_block!("daemon.store.session_relation_graph.open", {
            graph_registry.resolve_owner_attachment(GraphDbOwnerRegistrationV1 {
                operation: registration,
                authority_attachment: Box::new(store_attachment),
            })
        })
    })
    .await
    .map_err(|error| session_registry_error("join session relation graph open", error.to_string()))?
    .map_err(|error| {
        session_registry_error("open session relation graph owner", error.to_string())
    })?;
    Ok((graph, store_target))
}

pub async fn close_retained_for_shutdown(
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
    pub fn graph_store_identity(
        &self,
    ) -> (StoreRuntimeBindingV1, VerifiedStoreLocatorV1) {
        (
            self.graph.binding().clone(),
            self.graph.verified_locator().clone(),
        )
    }
}

fn registration(
    cancellation: Arc<dyn GraphCancellation>,
    lifecycle_cancellation: Arc<dyn GraphCancellation>,
    authority: Arc<dyn RetainedGraphStoreLeaseV1>,
) -> GraphDbRegistration {
    GraphDbRegistration {
        authority_lease: authority,
        cancellation,
        lifecycle_cancellation,
        deadline: Instant::now() + GRAPH_OPEN_DEADLINE,
    }
}

struct GraphOpenTaskCancellationV1 {
    lifecycle: Arc<AtomicBool>,
    operation: ObservationCancellation,
}

impl GraphCancellation for GraphOpenTaskCancellationV1 {
    fn is_cancelled(&self) -> bool {
        self.lifecycle.load(std::sync::atomic::Ordering::Acquire) || self.operation.is_cancelled()
    }
}
