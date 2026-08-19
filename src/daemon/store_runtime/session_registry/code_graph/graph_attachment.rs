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
    // KNOWN DEFECT, deliberately fast-failing: this close requires an
    // unleased owner, but the retained memory-graph runtimes in the project
    // owner map still hold their standing operation leases at this point in
    // shutdown, so the close reports a typed Conflict on every shutdown and
    // the reconciliation join is skipped (the receipt carries this exact
    // detail). Retrying here only slows shutdown — the lease is structural,
    // not a settling race. The fix is ordering: retire the retained runtimes
    // (releasing their leases) before closing the session relation graphs.
    tokio::task::spawn_blocking(move || {
        graph_registry.close_retained_for_shutdown(&binding, &verified_locator)
    })
    .await
    .map_err(|error| session_registry_error("join graph shutdown close", error.to_string()))?
    .map(|_| ())
    .map_err(|error| session_registry_error("close graph runtime for shutdown", error.to_string()))
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
