use std::sync::{Arc, Weak, atomic::AtomicBool};

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

/// Cloneable, non-retaining route to one exact verified graph runtime.
///
/// The originating [`crate::db::Database`] constructs this proxy only after
/// validating and storing its exact graph binding. Clones retain immutable
/// relational identity and a weak runtime pointer; every graph operation
/// upgrades privately for only the duration of that call.
#[derive(Clone)]
pub struct VerifiedGraphRuntimeWeakProxyV1 {
    relational_binding: StoreRuntimeBindingV1,
    relational_verified_locator: VerifiedStoreLocatorV1,
    runtime: Weak<dyn VerifiedGraphRuntimePortV1>,
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
            runtime,
        }
    }

    /// Whether both proxies route to the same exact runtime allocation.
    ///
    /// This comparison does not upgrade or expose the runtime. It remains
    /// valid after the map owner has dropped and supports idempotent binding
    /// without treating two authorities with equal descriptors as one owner.
    #[must_use]
    pub fn shares_runtime_with(&self, other: &Self) -> bool {
        self.runtime.ptr_eq(&other.runtime)
    }

    fn runtime(&self) -> Result<Arc<dyn VerifiedGraphRuntimePortV1>, GraphDbError> {
        self.runtime
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
        if let Some(runtime) = self.runtime.upgrade() {
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
