use std::sync::{Arc, OnceLock, Weak, atomic::AtomicBool};

use tracedecay_graph_db::{
    GraphDbError, GraphGenerationManifest, GraphIdempotencyKey, GraphProjectionIdentity,
    VerifiedGraphSnapshot,
};
use tracedecay_store::{FactReadControl, StoreRuntimeBindingV1, VerifiedStoreLocatorV1};

/// The sole verified graph publication and recovery authority retained by a
/// canonical relational shard.
pub trait VerifiedGraphRuntimePortV1: Send + Sync {
    /// Exact relational runtime whose replay journal and verified-head CAS
    /// back this graph authority.
    fn relational_binding(&self) -> &StoreRuntimeBindingV1;

    fn relational_verified_locator(&self) -> &VerifiedStoreLocatorV1;

    /// Closes lifecycle admission for background reconciliation owned by this
    /// exact retained runtime. In-flight publication observes the same signal
    /// and remains joinable by its database task owner.
    fn cancel_reconciliation(&self);

    fn publish_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        idempotency_key: GraphIdempotencyKey,
        cancelled: Arc<AtomicBool>,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError>;

    /// Reconciles one canonical manifest under the retained runtime's daemon
    /// lifecycle. Memory commit and mount catch-up callers do not own a
    /// request cancellation token and must not fabricate one.
    fn reconcile_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        idempotency_key: GraphIdempotencyKey,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError>;

    /// Recovers the projection's verified head. A projection that has never
    /// published answers `Ok(None)`; an unmounted authority is not represented
    /// by an implementation of this port.
    fn verified_snapshot(
        &self,
        projection: &GraphProjectionIdentity,
        read_control: FactReadControl,
    ) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError>;
}

/// The proxy's private route to its runtime allocation.
///
/// `Bound` carries a weak pointer resolved at construction and proves graph
/// activation had completed when the proxy was minted. `DeferredActivation`
/// carries the originating database's shared activation cell and resolves it
/// on every operation, so a proxy bound during composition — before deferred
/// graph activation completes — starts answering the moment activation
/// publishes the runtime, without any rebind choreography. The cell only ever
/// holds a weak pointer, so neither variant retains the database, a Store
/// lease, or the graph map owner.
#[derive(Clone)]
enum VerifiedGraphRuntimeSlotV1 {
    Bound(Weak<dyn VerifiedGraphRuntimePortV1>),
    DeferredActivation(Arc<OnceLock<Weak<dyn VerifiedGraphRuntimePortV1>>>),
}

/// Cloneable, non-retaining route to one exact verified graph runtime.
///
/// The originating [`crate::db::Database`] constructs this proxy only after
/// validating its exact graph identity. Clones retain immutable relational
/// identity and a weak runtime route; every graph operation resolves and
/// upgrades privately for only the duration of that call. A proxy minted
/// before deferred graph activation completes answers every operation with a
/// typed unavailable state until activation publishes the runtime.
#[derive(Clone)]
pub struct VerifiedGraphRuntimeWeakProxyV1 {
    relational_binding: StoreRuntimeBindingV1,
    relational_verified_locator: VerifiedStoreLocatorV1,
    runtime: VerifiedGraphRuntimeSlotV1,
}

impl VerifiedGraphRuntimeWeakProxyV1 {
    pub(crate) fn new(
        relational_binding: StoreRuntimeBindingV1,
        relational_verified_locator: VerifiedStoreLocatorV1,
        runtime: Weak<dyn VerifiedGraphRuntimePortV1>,
    ) -> Self {
        Self {
            relational_binding,
            relational_verified_locator,
            runtime: VerifiedGraphRuntimeSlotV1::Bound(runtime),
        }
    }

    pub(crate) fn new_deferred(
        relational_binding: StoreRuntimeBindingV1,
        relational_verified_locator: VerifiedStoreLocatorV1,
        activation: Arc<OnceLock<Weak<dyn VerifiedGraphRuntimePortV1>>>,
    ) -> Self {
        Self {
            relational_binding,
            relational_verified_locator,
            runtime: VerifiedGraphRuntimeSlotV1::DeferredActivation(activation),
        }
    }

    /// Whether both proxies route to the same exact runtime allocation.
    ///
    /// This comparison does not upgrade or expose the runtime. It remains
    /// valid after the map owner has dropped and supports idempotent binding
    /// without treating two authorities with equal descriptors as one owner.
    /// Two deferred proxies over one activation cell share the eventual
    /// runtime even before activation resolves it; otherwise both sides must
    /// already resolve to one allocation.
    #[must_use]
    pub fn shares_runtime_with(&self, other: &Self) -> bool {
        if let (
            VerifiedGraphRuntimeSlotV1::DeferredActivation(own),
            VerifiedGraphRuntimeSlotV1::DeferredActivation(theirs),
        ) = (&self.runtime, &other.runtime)
            && Arc::ptr_eq(own, theirs)
        {
            return true;
        }
        match (self.resolved_weak(), other.resolved_weak()) {
            (Some(own), Some(theirs)) => own.ptr_eq(theirs),
            _ => false,
        }
    }

    fn resolved_weak(&self) -> Option<&Weak<dyn VerifiedGraphRuntimePortV1>> {
        match &self.runtime {
            VerifiedGraphRuntimeSlotV1::Bound(runtime) => Some(runtime),
            VerifiedGraphRuntimeSlotV1::DeferredActivation(activation) => activation.get(),
        }
    }

    fn runtime(&self) -> Result<Arc<dyn VerifiedGraphRuntimePortV1>, GraphDbError> {
        self.resolved_weak()
            .ok_or_else(|| {
                GraphDbError::unavailable(
                    "verified graph runtime activation has not completed for its memory store",
                )
            })?
            .upgrade()
            .ok_or_else(|| GraphDbError::unavailable("verified graph runtime owner is unavailable"))
    }
}

impl VerifiedGraphRuntimePortV1 for VerifiedGraphRuntimeWeakProxyV1 {
    fn relational_binding(&self) -> &StoreRuntimeBindingV1 {
        &self.relational_binding
    }

    fn relational_verified_locator(&self) -> &VerifiedStoreLocatorV1 {
        &self.relational_verified_locator
    }

    fn cancel_reconciliation(&self) {
        if let Some(runtime) = self.resolved_weak().and_then(Weak::upgrade) {
            runtime.cancel_reconciliation();
        }
    }

    fn publish_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        idempotency_key: GraphIdempotencyKey,
        cancelled: Arc<AtomicBool>,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        self.runtime()?
            .publish_verified_manifest(manifest, idempotency_key, cancelled)
    }

    fn reconcile_verified_manifest(
        &self,
        manifest: &GraphGenerationManifest,
        idempotency_key: GraphIdempotencyKey,
    ) -> Result<VerifiedGraphSnapshot, GraphDbError> {
        self.runtime()?
            .reconcile_verified_manifest(manifest, idempotency_key)
    }

    fn verified_snapshot(
        &self,
        projection: &GraphProjectionIdentity,
        read_control: FactReadControl,
    ) -> Result<Option<VerifiedGraphSnapshot>, GraphDbError> {
        self.runtime()?.verified_snapshot(projection, read_control)
    }
}
